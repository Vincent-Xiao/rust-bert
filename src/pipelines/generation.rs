// Copyright 2018 The Google AI Language Team Authors, Facebook AI Research authors.
// Copyright 2018 Google AI, Google Brain and Carnegie Mellon University Authors and the HuggingFace Inc. team.
// Copyright (c) 2018, NVIDIA CORPORATION.  All rights reserved.
// Copyright 2019 Guillaume Becquin
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//     http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! # Natural Language Generation pipeline
//! Generate language based on a prompt. GPT2 and GPT available as base models.
//! Include techniques such as beam search, top-k and nucleus sampling, temperature setting and repetition penalty.
//! Supports batch generation of sentences from several prompts. Sequences will be left-padded with the model's padding token if present, the unknown token otherwise.
//! This may impact the results and it is recommended to submit prompts of similar length for best results.
//! All resources for this model can be downloaded using the Python utility script included in this repository.
//! 1. Set-up a Python virtual environment and install dependencies (in ./requirements.txt)
//! 2. Run the conversion script python /utils/download-dependencies_gpt2.py (or /utils/download-dependencies_openaigpt.py)
//! The dependencies will be downloaded to the user's home directory, under ~/rustbert/gpt2 (~/rustbert/openai-gpt respectively)
//!
//! ```no_run
//!# use std::path::PathBuf;
//!# use tch::Device;
//!# fn main() -> failure::Fallible<()> {
//! use rust_bert::pipelines::generation::{GenerateConfig, GPT2Generator, LanguageGenerator};
//!# let mut home: PathBuf = dirs::home_dir().unwrap();
//!# home.push("rustbert");
//!# home.push("gpt2");
//!# let config_path = &home.as_path().join("config.json");
//!# let vocab_path = &home.as_path().join("vocab.txt");
//!# let merges_path = &home.as_path().join("merges.txt");
//!# let weights_path = &home.as_path().join("model.ot");
//! let device = Device::cuda_if_available();
//! let generate_config = GenerateConfig {
//!    max_length: 30,
//!    do_sample: true,
//!    num_beams: 5,
//!    temperature: 1.1,
//!    num_return_sequences: 3,
//!    ..Default::default()
//! };
//! let gpt2_generator = GPT2Generator::new(vocab_path, merges_path, config_path, weights_path,
//!                                         generate_config, device)?;
//!
//! let input_context = "The dog";
//! let second_input_context = "The cat was";
//! let output = gpt2_generator.generate(Some(vec!(input_context, second_input_context)), None);
//!# Ok(())
//!# }
//! ```
//!
//! Example output: \
//! ```no_run
//!# let output =
//! [
//!     "The dog's owners, however, did not want to be named. According to the lawsuit, the animal's owner, a 29-year",
//!     "The dog has always been part of the family. \"He was always going to be my dog and he was always looking out for me",
//!     "The dog has been able to stay in the home for more than three months now. \"It's a very good dog. She's",
//!     "The cat was discovered earlier this month in the home of a relative of the deceased. The cat\'s owner, who wished to remain anonymous,",
//!     "The cat was pulled from the street by two-year-old Jazmine.\"I didn't know what to do,\" she said",
//!     "The cat was attacked by two stray dogs and was taken to a hospital. Two other cats were also injured in the attack and are being treated."
//! ]
//!# ;
//!```


use tch::{Tensor, Device, nn, no_grad};
use rust_tokenizers::{Tokenizer, OpenAiGptTokenizer, OpenAiGptVocab, Vocab, Gpt2Tokenizer, Gpt2Vocab};
use std::path::Path;
use tch::kind::Kind::Int64;
use self::ordered_float::OrderedFloat;
use itertools::Itertools;
use crate::openai_gpt::OpenAIGPTLMHeadModel;
use crate::gpt2::{Gpt2Config, GPT2LMHeadModel, LMHeadModel};
use crate::Config;
use crate::pipelines::generation::private_generation_utils::PrivateLanguageGenerator;

extern crate ordered_float;

/// # Configuration for text generation
pub struct GenerateConfig {
    /// Minimum sequence length (default: 0)
    pub min_length: u64,
    /// Maximum sequence length (default: 20)
    pub max_length: u64,
    /// Sampling flag. If true, will perform top-k and/or nucleus sampling on generated tokens, otherwise greedy (deterministic) decoding (default: true)
    pub do_sample: bool,
    /// Early stopping flag indicating if the beam search should stop as soon as `num_beam` hypotheses have been generated (default: false)
    pub early_stopping: bool,
    /// Number of beams for beam search (default: 5)
    pub num_beams: u64,
    /// Temperature setting. Values higher than 1 will improve originality at the risk of reducing relevance (default: 1.0)
    pub temperature: f64,
    /// Top_k values for sampling tokens. Value higher than 0 will enable the feature (default: 0)
    pub top_k: u64,
    /// Top_p value for [Nucleus sampling, Holtzman et al.](http://arxiv.org/abs/1904.09751). Keep top tokens until cumulative probability reaches top_p (default: 0.9)
    pub top_p: f64,
    /// Repetition penalty (mostly useful for CTRL decoders). Values higher than 1 will penalize tokens that have been already generated. (default: 1.0)
    pub repetition_penalty: f64,
    /// Exponential penalty based on the length of the hypotheses generated (default: 1.0)
    pub length_penalty: f64,
    /// Number of allowed repetitions of n-grams. Values higher than 0 turn on this feature (default: 3)
    pub no_repeat_ngram_size: u64,
    /// Number of sequences to return for each prompt text (default: 1)
    pub num_return_sequences: u64,
}

impl Default for GenerateConfig {
    fn default() -> GenerateConfig {
        GenerateConfig {
            min_length: 0,
            max_length: 20,
            do_sample: true,
            early_stopping: false,
            num_beams: 5,
            temperature: 1.0,
            top_k: 0,
            top_p: 0.9,
            repetition_penalty: 1.0,
            length_penalty: 1.0,
            no_repeat_ngram_size: 3,
            num_return_sequences: 1,
        }
    }
}

impl GenerateConfig {
    fn validate(&self) {
        assert!(self.temperature > 0f64, "temperature must positive");
        assert!((self.top_p >= 0f64) & (self.top_p <= 1f64), "top_p must be 0 and 1");
        assert!(self.repetition_penalty >= 1f64, "repetition_penalty must be greater than 1");
        assert!(self.length_penalty > 0f64, "length_penalty must be strictly greater than 0");
        assert!(self.num_return_sequences > 0u64, "num_return_sequences must be strictly greater than 0");
        assert!(self.num_beams > 0u64, "num_beams must be strictly greater than 0");

        if !self.do_sample {
            if self.num_beams == 1 {
                assert_eq!(self.num_return_sequences, 1, "num_return_sequences must be set to 1 for greedy decoding")
            } else {
                assert!(self.num_beams >= self.num_return_sequences, "num_return_sequences must be lower than the number of beams")
            }
        }
    }
}

/// # Language generation model based on the GPT architecture
pub struct OpenAIGenerator {
    model: OpenAIGPTLMHeadModel,
    tokenizer: OpenAiGptTokenizer,
    var_store: nn::VarStore,
    generate_config: GenerateConfig,
    bos_token_id: Option<i64>,
    eos_token_ids: Option<Vec<i64>>,
    pad_token_id: Option<i64>,
}

impl OpenAIGenerator {
    /// Build a new `OpenAIGenerator`
    ///
    /// # Arguments
    ///
    /// * `vocab_path` - Path to the model vocabulary, expected to have a structure following the [Transformers library](https://github.com/huggingface/transformers) convention
    /// * `merges_path` - Path to the bpe merges, expected to have a structure following the [Transformers library](https://github.com/huggingface/transformers) convention
    /// * `config_path` - Path to the model configuration, expected to have a structure following the [Transformers library](https://github.com/huggingface/transformers) convention
    /// * `weights_path` - Path to the model weight files. These need to be converted form the `.bin` to `.ot` format using the utility script provided.
    /// * `device` - Device to run the model on, e.g. `Device::Cpu` or `Device::Cuda(0)`
    ///
    /// # Example
    ///
    /// ```no_run
    ///# use std::path::PathBuf;
    ///# use tch::Device;
    ///# fn main() -> failure::Fallible<()> {
    /// use rust_bert::pipelines::generation::{GenerateConfig, OpenAIGenerator};
    ///# let mut home: PathBuf = dirs::home_dir().unwrap();
    ///# home.push("rustbert");
    ///# home.push("openai-gpt");
    ///# let config_path = &home.as_path().join("config.json");
    ///# let vocab_path = &home.as_path().join("vocab.txt");
    ///# let merges_path = &home.as_path().join("merges.txt");
    ///# let weights_path = &home.as_path().join("model.ot");
    /// let device = Device::cuda_if_available();
    /// let generate_config = GenerateConfig {
    ///    max_length: 30,
    ///    do_sample: true,
    ///    num_beams: 5,
    ///    temperature: 1.1,
    ///    num_return_sequences: 3,
    ///    ..Default::default()
    /// };
    /// let gpt_generator = OpenAIGenerator::new(vocab_path, merges_path, config_path, weights_path,
    ///                                          generate_config, device)?;
    ///# Ok(())
    ///# }
    /// ```
    ///
    pub fn new(vocab_path: &Path, merges_path: &Path, config_path: &Path, weight_path: &Path,
               generate_config: GenerateConfig, device: Device)
               -> failure::Fallible<OpenAIGenerator> {
        generate_config.validate();
        let mut var_store = nn::VarStore::new(device);
        let tokenizer = OpenAiGptTokenizer::from_file(vocab_path.to_str().unwrap(), merges_path.to_str().unwrap(), true);
        let config = Gpt2Config::from_file(config_path);
        let model = OpenAIGPTLMHeadModel::new(&var_store.root(), &config);
        var_store.load(weight_path)?;

        let bos_token_id = None;
        let eos_token_ids = None;
        let pad_token_id = None;

        Ok(OpenAIGenerator { model, tokenizer, var_store, generate_config, bos_token_id, eos_token_ids, pad_token_id })
    }
}

impl PrivateLanguageGenerator<OpenAIGPTLMHeadModel, OpenAiGptVocab, OpenAiGptTokenizer> for OpenAIGenerator {
    fn get_model(&self) -> &OpenAIGPTLMHeadModel { &self.model }
    fn get_tokenizer(&self) -> &OpenAiGptTokenizer { &self.tokenizer }
    fn get_var_store(&self) -> &nn::VarStore { &self.var_store }
    fn get_config(&self) -> &GenerateConfig { &self.generate_config }
    fn get_bos_id(&self) -> &Option<i64> { &self.bos_token_id }
    fn get_eos_ids(&self) -> &Option<Vec<i64>> { &self.eos_token_ids }
    fn get_pad_id(&self) -> &Option<i64> { &self.pad_token_id }
}

impl LanguageGenerator<OpenAIGPTLMHeadModel, OpenAiGptVocab, OpenAiGptTokenizer> for OpenAIGenerator {}

/// # Language generation model based on the GPT2 architecture
pub struct GPT2Generator {
    model: GPT2LMHeadModel,
    tokenizer: Gpt2Tokenizer,
    var_store: nn::VarStore,
    generate_config: GenerateConfig,
    bos_token_id: Option<i64>,
    eos_token_ids: Option<Vec<i64>>,
    pad_token_id: Option<i64>,
}

impl GPT2Generator {
    /// Build a new `GPT2Generator`
    ///
    /// # Arguments
    ///
    /// * `vocab_path` - Path to the model vocabulary, expected to have a structure following the [Transformers library](https://github.com/huggingface/transformers) convention
    /// * `merges_path` - Path to the bpe merges, expected to have a structure following the [Transformers library](https://github.com/huggingface/transformers) convention
    /// * `config_path` - Path to the model configuration, expected to have a structure following the [Transformers library](https://github.com/huggingface/transformers) convention
    /// * `weights_path` - Path to the model weight files. These need to be converted form the `.bin` to `.ot` format using the utility script provided.
    /// * `device` - Device to run the model on, e.g. `Device::Cpu` or `Device::Cuda(0)`
    ///
    /// # Example
    ///
    /// ```no_run
    ///# use std::path::PathBuf;
    ///# use tch::Device;
    ///# fn main() -> failure::Fallible<()> {
    /// use rust_bert::pipelines::generation::{GenerateConfig, GPT2Generator};
    ///# let mut home: PathBuf = dirs::home_dir().unwrap();
    ///# home.push("rustbert");
    ///# home.push("gpt2");
    ///# let config_path = &home.as_path().join("config.json");
    ///# let vocab_path = &home.as_path().join("vocab.txt");
    ///# let merges_path = &home.as_path().join("merges.txt");
    ///# let weights_path = &home.as_path().join("model.ot");
    /// let device = Device::cuda_if_available();
    /// let generate_config = GenerateConfig {
    ///    max_length: 30,
    ///    do_sample: true,
    ///    num_beams: 5,
    ///    temperature: 1.1,
    ///    num_return_sequences: 3,
    ///    ..Default::default()
    /// };
    /// let gpt2_generator = GPT2Generator::new(vocab_path, merges_path, config_path, weights_path,
    ///                                         generate_config, device)?;
    ///# Ok(())
    ///# }
    /// ```
    ///
    pub fn new(vocab_path: &Path, merges_path: &Path, config_path: &Path, weight_path: &Path,
               generate_config: GenerateConfig, device: Device)
               -> failure::Fallible<GPT2Generator> {
        generate_config.validate();
        let mut var_store = nn::VarStore::new(device);
        let tokenizer = Gpt2Tokenizer::from_file(vocab_path.to_str().unwrap(), merges_path.to_str().unwrap(), false);
        let config = Gpt2Config::from_file(config_path);
        let model = GPT2LMHeadModel::new(&var_store.root(), &config);
        var_store.load(weight_path)?;

        let bos_token_id = Some(tokenizer.vocab().token_to_id(Gpt2Vocab::bos_value()));
        let eos_token_ids = Some(vec!(tokenizer.vocab().token_to_id(Gpt2Vocab::eos_value())));
        let pad_token_id = None;

        Ok(GPT2Generator { model, tokenizer, var_store, generate_config, bos_token_id, eos_token_ids, pad_token_id })
    }
}

impl PrivateLanguageGenerator<GPT2LMHeadModel, Gpt2Vocab, Gpt2Tokenizer> for GPT2Generator {
    fn get_model(&self) -> &GPT2LMHeadModel { &self.model }
    fn get_tokenizer(&self) -> &Gpt2Tokenizer { &self.tokenizer }
    fn get_var_store(&self) -> &nn::VarStore { &self.var_store }
    fn get_config(&self) -> &GenerateConfig { &self.generate_config }
    fn get_bos_id(&self) -> &Option<i64> { &self.bos_token_id }
    fn get_eos_ids(&self) -> &Option<Vec<i64>> { &self.eos_token_ids }
    fn get_pad_id(&self) -> &Option<i64> { &self.pad_token_id }

    fn prepare_inputs_for_generation(&self, input_ids: Tensor, past: Option<Vec<Tensor>>, _attention_mask: Tensor) -> (Tensor, Option<Vec<Tensor>>) {
        if past.is_some() {
            (input_ids.select(1, -1).unsqueeze(-1), past)
        } else {
            (input_ids, past)
        }
    }
}

mod private_generation_utils {
    use crate::gpt2::LMHeadModel;
    use rust_tokenizers::{Vocab, Tokenizer, TruncationStrategy};
    use tch::{nn, Tensor};
    use rust_tokenizers::preprocessing::tokenizer::tokenization_utils::truncate_sequences;
    use std::collections::HashMap;
    use tch::kind::Kind::{Int64, Float, Bool};
    use std::cmp::{min, max};
    use crate::pipelines::generation::{BeamHypotheses, GenerateConfig};
    use itertools::Itertools;
    use super::ordered_float::OrderedFloat;

    pub trait PrivateLanguageGenerator<T: LMHeadModel, V: Vocab, U: Tokenizer<V>> {
        fn get_model(&self) -> &T;
        fn get_tokenizer(&self) -> &U;
        fn get_var_store(&self) -> &nn::VarStore;
        fn get_config(&self) -> &GenerateConfig;
        fn get_bos_id(&self) -> &Option<i64>;
        fn get_eos_ids(&self) -> &Option<Vec<i64>>;
        fn get_pad_id(&self) -> &Option<i64>;

        fn prepare_inputs_for_generation(&self, input_ids: Tensor, past: Option<Vec<Tensor>>, _attention_mask: Tensor) -> (Tensor, Option<Vec<Tensor>>) {
            (input_ids, past)
        }

        fn encode_prompt_text(&self, prompt_text: Vec<&str>, max_len: u64, pad_token_id: Option<i64>) -> Tensor {
            let tokens = self.get_tokenizer().tokenize_list(prompt_text);
            let token_ids = tokens
                .into_iter()
                .map(|prompt_tokens| self.get_tokenizer().convert_tokens_to_ids(&prompt_tokens))
                .collect::<Vec<Vec<i64>>>();

            let num_truncated_tokens = token_ids
                .iter()
                .map(|token_ids| if token_ids.len() > max_len as usize { token_ids.len() - max_len as usize } else { 0 })
                .collect::<Vec<usize>>();

            let token_ids = token_ids
                .into_iter()
                .zip(num_truncated_tokens)
                .map(|(tokens, num_truncated_tokens)| truncate_sequences(tokens,
                                                                         None,
                                                                         num_truncated_tokens,
                                                                         &TruncationStrategy::LongestFirst,
                                                                         0).unwrap().0)
                .collect::<Vec<Vec<i64>>>();

            let max_len = token_ids.iter().map(|input| input.len()).max().unwrap();

            let pad_token = match pad_token_id {
                Some(value) => value,
                None => self.get_tokenizer().vocab().token_to_id(V::unknown_value())
            };

            let token_ids = token_ids
                .into_iter()
                .map(|input| {
                    let mut temp = vec![pad_token; max_len - input.len()];
                    temp.extend(input);
                    temp
                })
                .map(|tokens| Tensor::of_slice(&tokens).to(self.get_var_store().device()))
                .collect::<Vec<Tensor>>();

            Tensor::stack(&token_ids, 0)
        }

        fn enforce_repetition_penalty(&self, next_token_logits: &mut Tensor, batch_size: i64, num_beams: u64, prev_output_tokens: &Tensor, repetition_penalty: f64) {
            for i in 0..(batch_size * num_beams as i64) {
                for token_position in 0..prev_output_tokens.get(i).size()[0] {
                    let token = prev_output_tokens.get(i).int64_value(&[token_position]);
                    let updated_value = &next_token_logits.double_value(&[i, token]);
                    if updated_value < &0f64 {
                        &next_token_logits.get(i).index_fill_(0, &Tensor::of_slice(&[token]).to_kind(Int64).to_device(next_token_logits.device()), updated_value * repetition_penalty);
                    } else {
                        &next_token_logits.get(i).index_fill_(0, &Tensor::of_slice(&[token]).to_kind(Int64).to_device(next_token_logits.device()), updated_value / repetition_penalty);
                    }
                }
            }
        }

        fn get_banned_tokens(&self, input_ids: &Tensor, no_repeat_ngram_size: i64, cur_len: i64) -> Vec<Vec<i64>> {
//        Ported from hugging face's transformers and fairseq (https://github.com/pytorch/fairseq/blob/master/fairseq/sequence_generator.py)
            if cur_len + 1 < no_repeat_ngram_size {
                vec!(vec!())
            } else {
                let num_hypothesis = *input_ids.size().first().unwrap();
                let mut banned_tokens: Vec<Vec<i64>> = Vec::with_capacity(num_hypothesis as usize);
                for hypothesis_index in 0..num_hypothesis {
                    let hypothesis_input_ids = input_ids.get(hypothesis_index);
                    let mut generated_ngram: HashMap<Vec<i64>, Vec<i64>> = HashMap::new();
                    let input: Vec<i64> = (0..hypothesis_input_ids.size1().unwrap()).collect();
                    let query = hypothesis_input_ids
                        .slice(0,
                               cur_len + 1 - no_repeat_ngram_size,
                               *hypothesis_input_ids.size().last().unwrap(),
                               1).iter::<i64>()
                        .unwrap()
                        .collect::<Vec<i64>>();
                    let ngram_indices: Vec<(i64, i64)> = input
                        .windows(3)
                        .map(|win| (*win.first().unwrap(), *win.last().unwrap()))
                        .collect();
                    for ngram in ngram_indices.into_iter() {
                        let ngram = hypothesis_input_ids
                            .slice(0, ngram.0, ngram.1 + 1, 1)
                            .iter::<i64>()
                            .unwrap()
                            .collect::<Vec<i64>>();
                        let key = ngram[..ngram.len() - 1].to_vec();
                        let value = *ngram.last().unwrap();
                        if generated_ngram.contains_key(&key) {
                            generated_ngram.get_mut(&key).unwrap().push(value)
                        } else {
                            generated_ngram.insert(key, vec!(value));
                        }
                    }
                    let hypothesis_banned_tokens = match generated_ngram.get(&query) {
                        Some(banned_tokens) => banned_tokens.clone(),
                        None => vec!()
                    };
                    banned_tokens.push(hypothesis_banned_tokens);
                }
                banned_tokens
            }
        }

        fn top_k_top_p_filtering(&self, logits: &mut Tensor, top_k: i64, top_p: f64, min_tokens_to_keep: i64) {
//        Nucleus and top-k filtering introduced by Holtzman et al. (http://arxiv.org/abs/1904.09751)
//        Ported from https://gist.github.com/thomwolf/1a5a29f6962089e871b94cbd09daf317
            let vocab_size = *logits.size().last().unwrap();
            if top_k > 0 {
                let top_k = vocab_size - min(max(top_k, min_tokens_to_keep), vocab_size);
                let (_, indices_to_remove) = logits.topk(top_k, -1, false, false);
                for index in 0..*logits.size().first().unwrap() {
                    &logits.get(index).index_fill_(0, &indices_to_remove.get(index), std::f64::NEG_INFINITY);
                }
            }

            if top_p < 1f64 {
                let (sorted_logits, sorted_indices) = logits.sort(-1, true);
                let cumulative_probabilities = sorted_logits.softmax(-1, Float).cumsum(-1, Float);
                let mut sorted_indices_to_remove = cumulative_probabilities.ge(top_p).to_kind(Int64);
                if min_tokens_to_keep > 1 {
                    &sorted_indices_to_remove.index_fill_(1, &Tensor::arange1(0, min_tokens_to_keep + 1, (Int64, logits.device())), 0);
                }
                let _ = sorted_indices_to_remove.index_copy_(1,
                                                             &Tensor::arange1(1, vocab_size, (Int64, logits.device())),
                                                             &sorted_indices_to_remove.slice(1, 0, vocab_size - 1, 1).copy());
                let _ = sorted_indices_to_remove.index_fill_(1, &Tensor::of_slice(&[0]).to_kind(Int64).to_device(sorted_indices_to_remove.device()), 0);
                let indices_to_remove = sorted_indices_to_remove.scatter(1, &sorted_indices, &sorted_indices_to_remove).to_kind(Bool);
                let _ = logits.masked_fill_(&indices_to_remove, std::f64::NEG_INFINITY);
            }
        }

        fn generate_no_beam_search(&self, input_ids: Tensor, cur_len: i64, min_length: i64, max_length: i64, do_sample: bool,
                                   temperature: f64, top_k: i64, top_p: f64, repetition_penalty: f64, no_repeat_ngram_size: i64,
                                   pad_token_id: Option<i64>, eos_token_ids: Option<Vec<i64>>,
                                   batch_size: i64, attention_mask: Tensor) -> Tensor {
            let mut unfinished_sentences = Tensor::ones(&[batch_size], (Int64, self.get_var_store().device()));
            let mut sentence_lengths: Tensor = Tensor::ones(&[batch_size], (Int64, self.get_var_store().device())) * max_length as i64;
            let mut attention_mask = attention_mask.copy();
            let mut input_ids = input_ids.copy();
            let mut past: Option<Vec<Tensor>> = None;
            let mut outputs: Tensor;
            let mut current_length = cur_len;

            while current_length < max_length {
                let (prepared_input, prepared_past) = self.prepare_inputs_for_generation(input_ids.copy(), past, attention_mask.copy());
                let temp = self.get_model().forward_t(&Some(prepared_input), &prepared_past, &None, &None, &None, &None, false).unwrap();
                outputs = temp.0;
                past = temp.1;
                let mut next_token_logits = outputs.select(1, -1);

//            Reduce probability for repeated inputs
                if repetition_penalty > 1f64 {
                    self.enforce_repetition_penalty(&mut next_token_logits, batch_size, 1, &input_ids, repetition_penalty)
                }

//            Get banned tokens and set their probability to 0
                let banned_tokens = self.get_banned_tokens(&input_ids, no_repeat_ngram_size as i64, current_length as i64);
                for (batch_index, index_banned_token) in (0..banned_tokens.len() as i64).zip(banned_tokens) {
                    &next_token_logits.get(batch_index).index_fill_(0, &Tensor::of_slice(&index_banned_token).to_device(next_token_logits.device()), std::f64::NEG_INFINITY);
                }

//            Do not allow eos token if min length is not reached
                if (&eos_token_ids.is_some()) & (current_length < min_length) {
                    &next_token_logits.index_fill_(1, &Tensor::of_slice(eos_token_ids.as_ref().unwrap()), std::f64::NEG_INFINITY);
                }

//            Top-k and top-p sampling
                let next_token = if do_sample {
                    if temperature > 1f64 {
                        next_token_logits = next_token_logits / temperature;
                    }
                    self.top_k_top_p_filtering(&mut next_token_logits, top_k as i64, top_p, 1);
                    let probabilities = next_token_logits.softmax(-1, Float);
                    probabilities.multinomial(1, false).squeeze1(1)
                } else {
                    next_token_logits.argmax(-1, false)
                };

//            Add tokens to unfinished sentences
                let tokens_to_add = match &eos_token_ids {
                    Some(_) => next_token * &unfinished_sentences - pad_token_id.unwrap() * (&unfinished_sentences - 1),
                    None => next_token
                };

                input_ids = Tensor::cat(&[input_ids, tokens_to_add.unsqueeze(-1)], -1);

                if eos_token_ids.is_some() {
                    for eos_token_id in eos_token_ids.as_ref().unwrap() {
                        let sentence_with_eos = tokens_to_add.eq(*eos_token_id).to_kind(Int64);
                        let sentence_with_eos: Tensor = sentence_with_eos * &unfinished_sentences;
                        let _ = sentence_lengths.masked_fill_(&sentence_with_eos.to_kind(Bool).to_device(sentence_lengths.device()), current_length as i64 + 1);
                        unfinished_sentences = -unfinished_sentences * (sentence_with_eos - 1);
                    }
                    if i64::from(unfinished_sentences.max()) == 0 {
                        break;
                    }
                }

                attention_mask = Tensor::cat(&[attention_mask.as_ref(), Tensor::ones(&[*attention_mask.size().first().unwrap(), 1],
                                                                                     (Int64, attention_mask.device())).as_ref()], -1);
                current_length += 1;
            }

            let decoded = if i64::from(&sentence_lengths.min().ne1(&sentence_lengths.max())) > 0 {
                match pad_token_id {
                    Some(pad_value) => {
                        let decoded: Tensor = Tensor::ones(&[batch_size, i64::from(sentence_lengths.max())], (Int64, input_ids.device())) * pad_value;
                        for hypothesis_index in 0..*input_ids.size().first().unwrap() {
                            let _ = decoded.get(hypothesis_index).index_copy_(0,
                                                                              &Tensor::arange1(0,
                                                                                               i64::from(sentence_lengths.get(hypothesis_index)),
                                                                                               (Int64, input_ids.device())),
                                                                              &input_ids.get(hypothesis_index));
                        }
                        decoded
                    }
                    None => input_ids
                }
            } else {
                input_ids
            };
            decoded
        }

        fn generate_beam_search(&self, input_ids: Tensor, cur_len: i64, min_length: i64, max_length: i64, do_sample: bool, early_stopping: bool,
                                temperature: f64, top_k: i64, top_p: f64, repetition_penalty: f64, no_repeat_ngram_size: i64,
                                pad_token_id: Option<i64>, eos_token_ids: Option<Vec<i64>>,
                                batch_size: i64, num_return_sequences: i64, length_penalty: f64, num_beams: i64, attention_mask: Tensor) -> Tensor {
            let mut hypotheses = (0..batch_size)
                .map(|_| BeamHypotheses::new(num_beams, max_length, length_penalty, early_stopping))
                .collect::<Vec<BeamHypotheses>>();

            let vocab_size = self.get_tokenizer().vocab().values().len() as i64;
            let beam_scores = Tensor::zeros(&[batch_size, num_beams], (Float, self.get_var_store().device()));
            if !do_sample {
                let _ = beam_scores.slice(1, 1, *beam_scores.size().last().unwrap(), 1).fill_(std::f64::NEG_INFINITY);
            }

            let mut beam_scores = beam_scores.view_(&[-1]);
            let mut beam_tokens: Tensor;
            let mut beam_indices: Tensor;
            let mut past: Option<Vec<Tensor>> = None;
            let mut done = vec!(false; batch_size as usize);

            let mut attention_mask = attention_mask.copy();
            let mut input_ids = input_ids.copy();
            let mut outputs: Tensor;
            let mut current_length = cur_len;

            while current_length < max_length {
                let (prepared_input, prepared_past) = self.prepare_inputs_for_generation(input_ids.copy(), past, attention_mask.copy());
                let temp = self.get_model().forward_t(&Some(prepared_input), &prepared_past, &None, &None, &None, &None, false).unwrap();
                outputs = temp.0;
                past = temp.1;
                let mut next_token_logits = outputs.select(1, -1);

//            Reduce probability for repeated inputs
                if repetition_penalty > 1f64 {
                    self.enforce_repetition_penalty(&mut next_token_logits, batch_size, 1, &input_ids, repetition_penalty)
                }

                if temperature > 1f64 {
                    next_token_logits = next_token_logits / temperature;
                }

                let mut scores = next_token_logits.log_softmax(-1, Float);

//            Do not allow eos token if min length is not reached
                if (&eos_token_ids.is_some()) & (current_length < min_length) {
                    &scores.index_fill_(1, &Tensor::of_slice(eos_token_ids.as_ref().unwrap()), std::f64::NEG_INFINITY);
                }

//            Get banned tokens and set their probability to 0
                let banned_tokens = self.get_banned_tokens(&input_ids, no_repeat_ngram_size as i64, current_length as i64);
                for (batch_index, index_banned_token) in (0..banned_tokens.len() as i64).zip(banned_tokens) {
                    &scores.get(batch_index).index_fill_(0, &Tensor::of_slice(&index_banned_token).to_device(next_token_logits.device()), std::f64::NEG_INFINITY);
                }

                let (next_scores, next_tokens) = if do_sample {
                    let mut _scores: Tensor = &scores + &beam_scores.unsqueeze(-1).expand_as(&scores);
                    self.top_k_top_p_filtering(&mut _scores, top_k as i64, top_p, 2);
                    let _scores = _scores.contiguous().view((batch_size, num_beams * vocab_size));

                    let probabilities = _scores.softmax(-1, Float);
                    let next_tokens = probabilities.multinomial(2 * num_beams, false);
                    let next_scores = _scores.gather(-1, &next_tokens, false);
                    let (next_scores, next_scores_indices) = next_scores.sort(1, true);
                    let next_tokens = next_tokens.gather(-1, &next_scores_indices, false);
                    (next_scores, next_tokens)
                } else {
                    let next_scores: Tensor = &scores + &beam_scores.unsqueeze(-1).expand_as(&scores);
                    let next_scores = next_scores.contiguous().view((batch_size, num_beams * vocab_size));
                    next_scores.topk(2 * num_beams, 1, true, true)
                };


                let mut next_batch_beam: Vec<(f64, i64, i64)> = vec!();

                for batch_index in 0..batch_size {
                    if done[batch_index as usize] {
                        assert!(hypotheses[batch_index as usize].len() >= num_beams,
                                "Batch cannot be completed if all beams have not been generated");
                        assert!(eos_token_ids.is_some() & pad_token_id.is_some(),
                                "EOS and Padding tokens need to be defined if the number of generated \
                            beams is greater than the target number fo beams");
                        next_batch_beam.append(&mut
                            (0..num_beams).map(|_| (0f64, pad_token_id.unwrap(), 0i64)).collect::<Vec<(f64, i64, i64)>>()
                        );
                    }

                    let mut next_sentence_beam: Vec<(f64, i64, i64)> = vec!();

                    let mut beam_token_rank = 0;
                    let beam_token_rank_max_value = *next_tokens.get(batch_index).size().first().unwrap() - 1;
                    loop {
                        let beam_token_id = next_tokens.int64_value(&[batch_index, beam_token_rank]);
                        let beam_token_score = next_scores.double_value(&[batch_index, beam_token_rank]);
                        let beam_id = beam_token_id / vocab_size;
                        let token_id = beam_token_id % vocab_size;

                        let effective_beam_id = batch_index * num_beams + beam_id;

                        if eos_token_ids.as_ref().is_some() {
                            if eos_token_ids.as_ref().unwrap().contains(&token_id) {
                                if beam_token_rank > num_beams {
                                    continue;
                                }
                                hypotheses[batch_index as usize].add(input_ids.get(effective_beam_id).copy(), beam_token_score)
                            } else {
                                next_sentence_beam.push((beam_token_score, token_id, effective_beam_id));
                            }
                        } else {
                            next_sentence_beam.push((beam_token_score, token_id, effective_beam_id));
                        }

                        if (next_sentence_beam.len() as i64 == num_beams) |
                            (beam_token_rank == beam_token_rank_max_value) {
                            break;
                        }

                        beam_token_rank += 1;
                    }

                    done[batch_index as usize] = done[batch_index as usize] |
                        hypotheses[batch_index as usize].is_done(
                            f64::from(next_scores.get(batch_index).max()),
                            current_length);

                    assert_eq!(next_sentence_beam.len() as i64, num_beams, "Beam incomplete");
                    next_batch_beam.append(&mut next_sentence_beam);
                }

                if done.iter().all(|&x| x) {
                    break;
                }
                beam_scores = Tensor::of_slice(&next_batch_beam.iter().map(|(score, _, _)| *score).collect_vec()).to(input_ids.device());
                beam_tokens = Tensor::of_slice(&next_batch_beam.iter().map(|(_, token, _)| *token).collect_vec()).to(input_ids.device());
                beam_indices = Tensor::of_slice(&next_batch_beam.iter().map(|(_, _, index)| *index).collect_vec()).to(input_ids.device());

                input_ids = input_ids.index_select(0, &beam_indices);
                input_ids = Tensor::cat(&[input_ids, beam_tokens.unsqueeze(1)], -1);

                past = match past {
                    Some(past_values) => Some(self.reorder_cache(past_values, &beam_indices)),
                    None => None
                };

                attention_mask = Tensor::cat(&[attention_mask.as_ref(), Tensor::ones(&[*attention_mask.size().first().unwrap(), 1],
                                                                                     (Int64, attention_mask.device())).as_ref()], -1);
                current_length += 1
            }

            let mut batch_index = 0i64;

            loop {
                if done[batch_index as usize] {
                    continue;
                }
                for beam_index in 0..num_beams {
                    let effective_beam_id = batch_index * num_beams + beam_index;
                    let final_score = f64::from(beam_scores.get(effective_beam_id));
                    let final_tokens = input_ids.get(effective_beam_id);
                    hypotheses[batch_index as usize].add(final_tokens, final_score);
                }
                batch_index += 1;
                if batch_index == batch_size {
                    break;
                }
            }

            let (output_batch_size, output_num_return_sequences_per_batch) = if do_sample {
                (batch_size, 1)
            } else {
                (batch_size * num_return_sequences, num_return_sequences)
            };

            let mut sentence_lengths = Tensor::zeros(&[output_batch_size], (Int64, input_ids.device()));
            let mut best_ids = vec!();

            for (hypothesis_index, hypothesis) in hypotheses.iter().enumerate() {
                let mut sorted_hypotheses = hypothesis.clone();
                &sorted_hypotheses.beams.sort_by_key(|(score, _)| OrderedFloat(*score));
                for j in 0..output_num_return_sequences_per_batch {
                    let effective_batch_index = output_num_return_sequences_per_batch * hypothesis_index as i64 + j;
                    let (_, best_hyp) = sorted_hypotheses.beams.pop().unwrap();
                    let _ = sentence_lengths.index_fill_(0,
                                                         &Tensor::of_slice(&[effective_batch_index]).to(sentence_lengths.device()),
                                                         *best_hyp.size().first().unwrap());
                    best_ids.push(best_hyp);
                }
            }

            let decoded = if i64::from(sentence_lengths.max()) != i64::from(sentence_lengths.min()) {
                let sentence_max_length = min(i64::from(sentence_lengths.max()) + 1, max_length);
                let decoded: Tensor = Tensor::ones(&[output_batch_size, sentence_max_length], (Int64, input_ids.device())) * pad_token_id.unwrap();
                for hypothesis_index in 0..best_ids.len() {
                    let _ = decoded
                        .get(hypothesis_index as i64)
                        .index_copy_(0,
                                     &Tensor::arange1(0,
                                                      i64::from(sentence_lengths.get(hypothesis_index as i64)),
                                                      (Int64, input_ids.device())),
                                     &best_ids[hypothesis_index]);
                    let sentence_length = i64::from(sentence_lengths.get(hypothesis_index as i64));
                    if sentence_length < max_length {
                        let _ = decoded
                            .get(hypothesis_index as i64)
                            .index_fill_(0, &Tensor::of_slice(&[sentence_length]).to_device(input_ids.device()), eos_token_ids.as_ref().unwrap()[0]);
                    }
                }
                decoded
            } else {
                Tensor::stack(&best_ids, 0).to_kind(Int64).to(input_ids.device())
            };
            decoded
        }

        fn reorder_cache(&self, past: Vec<Tensor>, beam_indices: &Tensor) -> Vec<Tensor> {
            let mut reordered_past = vec!();
            for layer_past in past.iter() {
                reordered_past.push(layer_past.index_select(1, beam_indices));
            }
            reordered_past
        }
    }
}

impl LanguageGenerator<GPT2LMHeadModel, Gpt2Vocab, Gpt2Tokenizer> for GPT2Generator {}

/// # Common trait for text generation models.
/// Main API for text generation
pub trait LanguageGenerator<T: LMHeadModel, V: Vocab, U: Tokenizer<V>>: PrivateLanguageGenerator<T, V, U> {

    /// Generate text based on a vector of promp texts.
    ///
    /// # Arguments
    ///
    /// * `prompt_texts` - `Option<Vec<&str>>` Optional vector of text prompts. An empty prompt to the model may be passed if the model implement a `bos_id`.
    /// * `attention_mask` - `Option<Tensor>` Optional attention mask to hide portions of the prompt.
    ///
    /// # Returns
    /// * `Vec<String>` Vector of generated strings based on the prompts of length *number_of_prompts* x *num_return_sequences*.
    ///
    /// # Example
    ///
    /// ```no_run
    ///# use std::path::PathBuf;
    ///# use tch::Device;
    ///# fn main() -> failure::Fallible<()> {
    /// use rust_bert::pipelines::generation::{GenerateConfig, GPT2Generator, LanguageGenerator};
    ///# let mut home: PathBuf = dirs::home_dir().unwrap();
    ///# home.push("rustbert");
    ///# home.push("gpt2");
    ///# let config_path = &home.as_path().join("config.json");
    ///# let vocab_path = &home.as_path().join("vocab.txt");
    ///# let merges_path = &home.as_path().join("merges.txt");
    ///# let weights_path = &home.as_path().join("model.ot");
    /// let device = Device::cuda_if_available();
    /// let generate_config = GenerateConfig {
    ///    max_length: 30,
    ///    do_sample: true,
    ///    num_beams: 5,
    ///    temperature: 1.1,
    ///    num_return_sequences: 3,
    ///    ..Default::default()
    /// };
    /// let gpt2_generator = GPT2Generator::new(vocab_path, merges_path, config_path, weights_path,
    ///                                         generate_config, device)?;
    /// let input_context = "The dog";
    /// let second_input_context = "The cat was";
    /// let output = gpt2_generator.generate(Some(vec!(input_context, second_input_context)), None);
    ///# Ok(())
    ///# }
    /// ```
    /// Example output: \
    /// ```no_run
    ///# let output =
    /// [
    ///     "The dog's owners, however, did not want to be named. According to the lawsuit, the animal's owner, a 29-year",
    ///     "The dog has always been part of the family. \"He was always going to be my dog and he was always looking out for me",
    ///     "The dog has been able to stay in the home for more than three months now. \"It's a very good dog. She's",
    ///     "The cat was discovered earlier this month in the home of a relative of the deceased. The cat\'s owner, who wished to remain anonymous,",
    ///     "The cat was pulled from the street by two-year-old Jazmine.\"I didn't know what to do,\" she said",
    ///     "The cat was attacked by two stray dogs and was taken to a hospital. Two other cats were also injured in the attack and are being treated."
    /// ]
    ///# ;
    ///```
    ///
    fn generate(&self, prompt_texts: Option<Vec<&str>>, attention_mask: Option<Tensor>)
                -> Vec<String> {
        let eos_token_ids = PrivateLanguageGenerator::get_eos_ids(self).clone();

        let config = PrivateLanguageGenerator::get_config(self);
        let do_sample = config.do_sample;
        let num_return_sequences = config.num_return_sequences;
        let num_beams = config.num_beams;
        let min_length = config.min_length;
        let max_length = config.max_length;
        let early_stopping = config.early_stopping;
        let temperature = config.temperature;
        let top_k = config.top_k;
        let top_p = config.top_p;
        let repetition_penalty = config.repetition_penalty;
        let length_penalty = config.length_penalty;
        let no_repeat_ngram_size = config.no_repeat_ngram_size;


        let pad_token_id = match self.get_pad_id() {
            Some(value) => Some(*value),
            None => match &eos_token_ids {
                Some(eos_ids) => Some(eos_ids[0]),
                None => None
            }
        };

        let input_ids = match prompt_texts {
            Some(text) => self.encode_prompt_text(text, max_length, pad_token_id),
            None => match self.get_bos_id() {
                Some(bos_id) => Tensor::ones(&[1, 1], (Int64, self.get_var_store().device())) * *bos_id,
                None => panic!("A model with a BOS token must be used to start generation with an empty input")
            }
        };

        let cur_len = *input_ids.size().last().unwrap();
        let batch_size = *input_ids.size().first().unwrap();

        let (effective_batch_size, effective_batch_mult) = match do_sample {
            true => (batch_size * num_return_sequences as i64, num_return_sequences as i64),
            false => (batch_size, 1)
        };

        let attention_mask = match attention_mask {
            Some(value) => value,
            None => {
                match self.get_pad_id() {
                    Some(pad_id) => input_ids.ne(*pad_id),
                    None => input_ids.ones_like()
                }
            }
        };

        let (input_ids, attention_mask) = if (num_return_sequences > 1) | (num_beams > 1) {
            (input_ids
                 .unsqueeze(1)
                 .expand(&[batch_size, effective_batch_mult * num_beams as i64, cur_len], true)
                 .contiguous()
                 .view((effective_batch_size * num_beams as i64, cur_len)),
             attention_mask
                 .unsqueeze(1)
                 .expand(&[batch_size, effective_batch_mult * num_beams as i64, cur_len], true)
                 .contiguous()
                 .view((effective_batch_size * num_beams as i64, cur_len))
            )
        } else {
            (input_ids, attention_mask)
        };

        let decoded = no_grad(|| {
            if num_beams > 1 {
                self.generate_beam_search(input_ids, cur_len, min_length as i64, max_length as i64, do_sample, early_stopping, temperature, top_k as i64, top_p, repetition_penalty,
                                          no_repeat_ngram_size as i64, pad_token_id, eos_token_ids, effective_batch_size, num_return_sequences as i64, length_penalty, num_beams as i64, attention_mask)
            } else {
                self.generate_no_beam_search(input_ids, cur_len, min_length as i64, max_length as i64, do_sample, temperature, top_k as i64, top_p, repetition_penalty,
                                             no_repeat_ngram_size as i64, pad_token_id, eos_token_ids, effective_batch_size, attention_mask)
            }
        });

        let num_sequences = *decoded.size().first().unwrap();
        let mut output = Vec::with_capacity(num_sequences as usize);
        for sequence_index in 0..num_sequences {
            output.push(self.get_tokenizer().decode(decoded
                                                        .as_ref()
                                                        .get(sequence_index)
                                                        .iter::<i64>()
                                                        .unwrap()
                                                        .collect::<Vec<i64>>(), true, true));
        }
        output
    }
}

#[derive(Debug)]
struct BeamHypotheses {
    max_length: i64,
    length_penalty: f64,
    early_stopping: bool,
    num_beams: i64,
    beams: Vec<(f64, Tensor)>,
    worst_score: f64,
}

impl Clone for BeamHypotheses {
    fn clone(&self) -> Self {
        BeamHypotheses {
            max_length: self.max_length,
            length_penalty: self.length_penalty,
            early_stopping: self.early_stopping,
            num_beams: self.num_beams,
            beams: self.beams
                .iter()
                .map(|(score, tensor)| (*score, tensor.copy()))
                .collect_vec(),
            worst_score: self.worst_score,
        }
    }
}

impl BeamHypotheses {
    fn new(num_beams: i64, max_length: i64, length_penalty: f64, early_stopping: bool) -> BeamHypotheses {
        BeamHypotheses {
            max_length: max_length - 1,
            length_penalty,
            early_stopping,
            num_beams,
            beams: Vec::with_capacity(num_beams as usize + 1),
            worst_score: std::f64::INFINITY,
        }
    }

    fn len(&self) -> i64 {
        self.beams.len() as i64
    }

    fn add(&mut self, hypothesis: Tensor, sum_log_probabilities: f64) {
        let score = sum_log_probabilities / ((*hypothesis.size().first().unwrap() as f64).powf(self.length_penalty));
        if (self.len() < self.num_beams) | (score > self.worst_score) {
            self.beams.push((score, hypothesis));
            if self.len() > self.num_beams {
                let (worst_score_position, _) = self.beams
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (score, _))| OrderedFloat(*score))
                    .unwrap();
                let _ = self.beams.remove(worst_score_position);
            }
            self.worst_score = self.beams.iter().min_by_key(|(score, _)| OrderedFloat(*score)).unwrap().0;
        }
    }

    fn is_done(&self, best_sum_log_probabilities: f64, current_length: i64) -> bool {
        if self.len() < self.num_beams {
            false
        } else if self.early_stopping {
            true
        } else {
            self.worst_score >= best_sum_log_probabilities / (current_length as f64).powf(self.length_penalty)
        }
    }
}