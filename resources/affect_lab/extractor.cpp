// SPDX-License-Identifier: MIT
// A small mean-difference control-vector extractor using llama.cpp's public
// API. Capture the last prompt token at l_out for each *language* layer. Layer
// names are retained exactly, so direction.N is injected at the same layer N.
// MTP, layer zero (not injectable), and the final language layer (its tensor
// can be renamed by the output graph) are excluded. Each pair starts with empty
// state.
#include <ggml-backend.h>
#include <ggml.h>
#include <gguf.h>
#include <llama.h>

#include <algorithm>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iostream>
#include <map>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

struct capture_state {
  int layers = 0;
  int embedding = 0;
  std::map<int, std::vector<float>> values;
  std::string error;
};

static int layer_index(const char *name) {
  if (std::strncmp(name, "l_out-", 6) != 0)
    return -1;
  char *end = nullptr;
  long value = std::strtol(name + 6, &end, 10);
  return end && *end == '\0' && value >= 0 && value < 4096 ? int(value) : -1;
}

static bool capture(ggml_tensor *tensor, bool ask, void *pointer) {
  auto &state = *static_cast<capture_state *>(pointer);
  const int layer = layer_index(tensor->name);
  const bool wanted = layer >= 1 && layer < state.layers - 1;
  if (ask)
    return wanted;
  if (!wanted)
    return true;
  if (tensor->type != GGML_TYPE_F32 || tensor->ne[0] != state.embedding ||
      tensor->ne[1] < 1 || tensor->nb[0] != sizeof(float)) {
    state.error =
        "Unexpected residual tensor layout at layer " + std::to_string(layer);
    return false;
  }
  auto &output = state.values[layer];
  output.resize(state.embedding);
  const size_t offset = size_t(tensor->ne[1] - 1) * tensor->nb[1];
  ggml_backend_tensor_get(tensor, output.data(), offset,
                          output.size() * sizeof(float));
  return true;
}

static std::string metadata(const llama_model *model, const std::string &key) {
  char buffer[256] = {};
  const int count =
      llama_model_meta_val_str(model, key.c_str(), buffer, sizeof(buffer));
  if (count < 0)
    return "";
  if (count >= int(sizeof(buffer)))
    throw std::runtime_error("Model metadata is too long: " + key);
  return buffer;
}

static std::vector<std::string> prompts(const std::string &path) {
  std::ifstream file(path);
  if (!file)
    throw std::runtime_error("Cannot read prompt file " + path);
  std::vector<std::string> result;
  std::string line;
  while (std::getline(file, line)) {
    if (line.empty())
      continue;
    std::string decoded;
    for (size_t index = 0; index < line.size(); ++index) {
      if (line[index] == '\\' && index + 1 < line.size()) {
        const char next = line[++index];
        if (next == 'n')
          decoded += '\n';
        else if (next == '\\')
          decoded += '\\';
        else
          throw std::runtime_error("Unsupported prompt escape");
      } else
        decoded += line[index];
    }
    result.push_back(decoded);
  }
  if (result.size() < 2 || result.size() > 64)
    throw std::runtime_error("Require 2..64 prompt pairs");
  return result;
}

static void evaluate(llama_context *context, const llama_vocab *vocab,
                     const std::string &text, capture_state &state) {
  const int needed = llama_tokenize(vocab, text.c_str(), int(text.size()),
                                    nullptr, 0, true, true);
  if (needed >= 0 || -needed > 1024)
    throw std::runtime_error("Prompt must contain 1..1024 tokens");
  std::vector<llama_token> tokens(-needed);
  const int count =
      llama_tokenize(vocab, text.c_str(), int(text.size()), tokens.data(),
                     int(tokens.size()), true, true);
  if (count <= 0)
    throw std::runtime_error("Tokenization failed");
  tokens.resize(count);
  state.values.clear();
  state.error.clear();
  llama_memory_clear(llama_get_memory(context), true);
  if (llama_decode(context, llama_batch_get_one(tokens.data(),
                                                int(tokens.size()))) != 0) {
    throw std::runtime_error("Prompt evaluation failed: " + state.error);
  }
  llama_synchronize(context);
  if (!state.error.empty())
    throw std::runtime_error(state.error);
  for (int layer = 1; layer < state.layers - 1; ++layer) {
    if (!state.values.count(layer))
      throw std::runtime_error("Missing residual activation at layer " +
                               std::to_string(layer));
  }
}

int main(int argc, char **argv) {
  try {
    std::map<std::string, std::string> options;
    for (int index = 1; index < argc; ++index) {
      const std::string flag = argv[index];
      if (flag == "--version") {
        std::cout << "ponderer-cvector/1 last-prompt-token matched mean; "
                     "native layer indices\n";
        return 0;
      }
      if (flag == "--offline")
        continue; // This executable has no network implementation.
      if (index + 1 >= argc)
        throw std::runtime_error("Missing value for " + flag);
      options[flag] = argv[++index];
    }
    for (const auto &key :
         {"--model", "--positive-file", "--negative-file", "--output"}) {
      if (!options.count(key))
        throw std::runtime_error(std::string("Missing ") + key);
    }
    if (options.count("--method") && options["--method"] != "mean")
      throw std::runtime_error("Only matched mean extraction is implemented");
    const auto positive = prompts(options["--positive-file"]);
    const auto negative = prompts(options["--negative-file"]);
    if (positive.size() != negative.size())
      throw std::runtime_error("Target and control counts differ");
    llama_backend_init();
    auto model_params = llama_model_default_params();
    model_params.n_gpu_layers =
        options.count("--gpu-layers") ? std::stoi(options["--gpu-layers"]) : 0;
    std::unique_ptr<llama_model, decltype(&llama_model_free)> model(
        llama_model_load_from_file(options["--model"].c_str(), model_params),
        llama_model_free);
    if (!model)
      throw std::runtime_error("Cannot load model with installed llama.cpp");
    const std::string arch = metadata(model.get(), "general.architecture");
    const std::string block_count =
        metadata(model.get(), arch + ".block_count");
    const std::string nextn =
        metadata(model.get(), arch + ".nextn_predict_layers");
    const int file_layers = block_count.empty()
                                ? llama_model_n_layer(model.get())
                                : std::stoi(block_count);
    capture_state state;
    state.layers =
        std::min(llama_model_n_layer(model.get()),
                 file_layers - (nextn.empty() ? 0 : std::stoi(nextn)));
    state.embedding = llama_model_n_embd(model.get());
    if (state.layers < 3 || state.embedding < 1)
      throw std::runtime_error("Unsupported model dimensions");
    auto context_params = llama_context_default_params();
    context_params.n_ctx = context_params.n_batch = context_params.n_ubatch =
        1024;
    context_params.n_seq_max = 1;
    context_params.n_threads = context_params.n_threads_batch =
        options.count("--threads") ? std::stoi(options["--threads"]) : 4;
    context_params.cb_eval = capture;
    context_params.cb_eval_user_data = &state;
    std::unique_ptr<llama_context, decltype(&llama_free)> context(
        llama_init_from_model(model.get(), context_params), llama_free);
    if (!context)
      throw std::runtime_error("Cannot initialize inference context");
    const llama_vocab *vocab = llama_model_get_vocab(model.get());
    std::vector<std::vector<float>> directions(
        state.layers - 2, std::vector<float>(state.embedding, 0));
    for (size_t pair = 0; pair < positive.size(); ++pair) {
      std::cout << "Evaluating pair " << pair + 1 << "/" << positive.size()
                << std::endl;
      evaluate(context.get(), vocab, positive[pair], state);
      auto target = state.values;
      evaluate(context.get(), vocab, negative[pair], state);
      for (int layer = 1; layer < state.layers - 1; ++layer) {
        for (int column = 0; column < state.embedding; ++column) {
          const float difference =
              target[layer][column] - state.values[layer][column];
          if (!std::isfinite(difference))
            throw std::runtime_error("Non-finite activation difference");
          directions[layer - 1][column] += difference / float(positive.size());
        }
      }
    }
    std::unique_ptr<ggml_context, decltype(&ggml_free)> tensors(
        ggml_init(
            {ggml_tensor_overhead() * size_t(state.layers), nullptr, true}),
        ggml_free);
    if (!tensors)
      throw std::runtime_error("Cannot allocate vector metadata");
    std::unique_ptr<gguf_context, decltype(&gguf_free)> output(
        gguf_init_empty(), gguf_free);
    gguf_set_val_str(output.get(), "general.architecture", "controlvector");
    gguf_set_val_str(output.get(), "controlvector.model_hint", arch.c_str());
    gguf_set_val_i32(output.get(), "controlvector.layer_count",
                     state.layers - 2);
    gguf_set_val_str(output.get(), "ponderer.extraction",
                     "last-prompt-token matched mean; native layer indices");
    for (int layer = 1; layer < state.layers - 1; ++layer) {
      auto &direction = directions[layer - 1];
      double norm_squared = 0;
      for (float value : direction)
        norm_squared += double(value) * value;
      const double norm = std::sqrt(norm_squared);
      if (!std::isfinite(norm) || norm < 1e-12)
        throw std::runtime_error("Degenerate direction at layer " +
                                 std::to_string(layer));
      for (float &value : direction)
        value = float(value / norm);
      auto *tensor =
          ggml_new_tensor_1d(tensors.get(), GGML_TYPE_F32, state.embedding);
      tensor->data = direction.data();
      ggml_set_name(tensor, ("direction." + std::to_string(layer)).c_str());
      gguf_add_tensor(output.get(), tensor);
    }
    if (!gguf_write_to_file(output.get(), options["--output"].c_str(), false))
      throw std::runtime_error("Cannot write vector GGUF");
    std::cout << "Wrote " << state.layers - 2 << " unit directions for "
              << state.layers << " language layers" << std::endl;
    // Models and contexts release before process exit; no modified model
    // weights.
    return 0;
  } catch (const std::exception &error) {
    std::cerr << "Vector extraction failed: " << error.what() << std::endl;
    return 1;
  }
}
