// Copyright 2018 Google AI and Google Brain team.
// Copyright 2020-present, the HuggingFace Inc. team.
// Copyright 2020 Guillaume Becquin
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//     http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

extern crate failure;

use tch::Device;
use rust_tokenizers::AlbertTokenizer;
use rust_bert::Config;
use rust_bert::resources::{Resource, download_resource, LocalResource};
use rust_bert::albert::AlbertConfig;


fn main() -> failure::Fallible<()> {
    //    Resources paths
    let config_resource = Resource::Local(LocalResource { local_path: "E:/Coding/cache/rustbert/albert-base-v2/config.json".parse().unwrap() });
    let vocab_resource = Resource::Local(LocalResource { local_path: "E:/Coding/cache/rustbert/albert-base-v2/spiece.model".parse().unwrap() });
    let weights_resource = Resource::Local(LocalResource { local_path: "E:/Coding/cache/rustbert/albert-base-v2/model.ot".parse().unwrap() });
    let config_path = download_resource(&config_resource)?;
    let vocab_path = download_resource(&vocab_resource)?;
    let _weights_path = download_resource(&weights_resource)?;

//    Set-up masked LM model
    let _device = Device::Cpu;
    // let mut vs = nn::VarStore::new(device);
    let _tokenizer: AlbertTokenizer = AlbertTokenizer::from_file(vocab_path.to_str().unwrap(), true, false);
    let _config = AlbertConfig::from_file(config_path);
//     let bert_model = BertForMaskedLM::new(&vs.root(), &config);
//     vs.load(weights_path)?;
//
// //    Define input
//     let input = ["Looks like one [MASK] is missing", "It was a very nice and [MASK] day"];
//     let tokenized_input = tokenizer.encode_list(input.to_vec(), 128, &TruncationStrategy::LongestFirst, 0);
//     let max_len = tokenized_input.iter().map(|input| input.token_ids.len()).max().unwrap();
//     let tokenized_input = tokenized_input.
//         iter().
//         map(|input| input.token_ids.clone()).
//         map(|mut input| {
//             input.extend(vec![0; max_len - input.len()]);
//             input
//         }).
//         map(|input|
//             Tensor::of_slice(&(input))).
//         collect::<Vec<_>>();
//     let input_tensor = Tensor::stack(tokenized_input.as_slice(), 0).to(device);
//
// //    Forward pass
//     let (output, _, _) = no_grad(|| {
//         bert_model
//             .forward_t(Some(input_tensor),
//                        None,
//                        None,
//                        None,
//                        None,
//                        &None,
//                        &None,
//                        false)
//     });
//
// //    Print masked tokens
//     let index_1 = output.get(0).get(4).argmax(0, false);
//     let index_2 = output.get(1).get(7).argmax(0, false);
//     let word_1 = tokenizer.vocab().id_to_token(&index_1.int64_value(&[]));
//     let word_2 = tokenizer.vocab().id_to_token(&index_2.int64_value(&[]));
//
//     println!("{}", word_1); // Outputs "person" : "Looks like one [person] is missing"
//     println!("{}", word_2);// Outputs "pear" : "It was a very nice and [pleasant] day"

    Ok(())
}