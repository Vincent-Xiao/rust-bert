/// Derived from https://github.com/MaartenGr/KeyBERT, shared under MIT License
///
/// Copyright (c) 2020, Maarten P. Grootendorst
/// Copyright (c) 2022, Guillaume Becquin
///
/// Permission is hereby granted, free of charge, to any person obtaining a copy
/// of this software and associated documentation files (the "Software"), to deal
/// in the Software without restriction, including without limitation the rights
/// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
/// copies of the Software, and to permit persons to whom the Software is
/// furnished to do so, subject to the following conditions:
///
/// The above copyright notice and this permission notice shall be included in all
/// copies or substantial portions of the Software.
///
/// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
/// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
/// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
/// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
/// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
/// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
/// SOFTWARE.
use crate::pipelines::keywords_extraction::tokenizer::StopWordsTokenizer;
#[cfg(feature = "remote")]
use crate::pipelines::sentence_embeddings::SentenceEmbeddingsModelType;
use crate::pipelines::sentence_embeddings::{
    SentenceEmbeddingsConfig, SentenceEmbeddingsModel, SentenceEmbeddingsSentenceBertConfig,
    SentenceEmbeddingsTokenizerConfig,
};
use crate::{Config, RustBertError};
use regex::Regex;
use rust_tokenizers::Offset;
use std::borrow::Cow;
use std::cmp::min;
use std::collections::{HashMap, HashSet};

/// # Keyword generated by a `KeywordExtractionModel`
#[derive(Debug, Clone)]
pub struct Keyword {
    /// String representation of the keyword
    pub text: String,
    /// Similarity score for the keyword
    pub score: f32,
    /// List of offsets where the keyword was found
    pub offsets: Vec<Offset>,
}

/// # Scoring function variants for keyword ranking
pub enum KeywordScorerType {
    /// Cosine similarity ranker, computing score as the dot product of the normalized
    /// vector representations for the document and keywords from the sentence embedding model
    CosineSimilarity,
    /// Maximal margin relevance ranker. The first keyword has the maximum cosine similarity with the
    /// document. Further keywords are incrementally chosen based on their similarity to the document
    /// and penalized by the maximum similarity to the keywords already identified, adjusted by a diversity
    /// factor. A high diversity (closer to 1.0) will give more importance to getting varied keywords, at the
    /// cost of less relevance to the original document.
    MaximalMarginRelevance,
    /// Maximum sum ranker. An original list of the top-N keywords is identified via cosine similarity.
    /// For all `N choose k` combinations of k keywords, the combination with the maximum internal
    /// distance (sum of all distance from a keyword to other keywords in the set) is chosen as the list
    /// of keywords to return. High values of `max_sum_candidates` will lead to a high number of keyword
    /// candidates and increase the computational cost / memory requirements.
    MaxSum,
}

/// # Configuration for Keyword extraction
pub struct KeywordExtractionConfig<'a> {
    /// `SentenceEmbeddingsConfig` defining the sentence embeddings model to use
    pub sentence_embeddings_config: SentenceEmbeddingsConfig,
    /// Optional list of tokenizer stopwords to exclude from the keywords candidate list. Default to a list of English stopwords.
    pub tokenizer_stopwords: Option<HashSet<&'a str>>,
    /// Optional tokenization regex pattern. Defaults to sequence of word characters.
    pub tokenizer_pattern: Option<Regex>,
    /// `KeywordScorerType` used to rank keywords.
    pub scorer_type: KeywordScorerType,
    /// N-gram range (inclusive) for keywords. (1, 2) would consider all 1 and 2 word gram for keyword candidates.
    pub ngram_range: (usize, usize),
    /// Number of keywords to return
    pub num_keywords: usize,
    /// Optional diversity parameter used for the `MaximalMarginRelevance` ranker, defaults to 0.5.
    /// A high diversity (closer to 1.0) will give more importance to getting varied keywords, at the
    /// cost of less relevance to the original document.
    pub diversity: Option<f64>,
    /// Optional number of candidate sets used for `MaxSum` ranker. Higher values are more likely to
    /// identify a global optimum for the ranker criterion, but are more likely to include sets that are less relevant to the
    /// input document. Larger values also have a higher computational and memory cost (N<sup>2</sup> scale)
    pub max_sum_candidates: Option<usize>,
}

#[cfg(feature = "remote")]
impl Default for KeywordExtractionConfig<'_> {
    fn default() -> Self {
        let sentence_embeddings_config =
            SentenceEmbeddingsConfig::from(SentenceEmbeddingsModelType::AllMiniLmL6V2);

        Self {
            sentence_embeddings_config,
            tokenizer_stopwords: None,
            tokenizer_pattern: None,
            scorer_type: KeywordScorerType::CosineSimilarity,
            ngram_range: (1, 1),
            num_keywords: 5,
            diversity: None,
            max_sum_candidates: None,
        }
    }
}

/// # KeywordExtractionModel to extract keywords from input texts
///
/// It contains a sentence embeddings model to compute word-document similarities,
/// a tokenizer to define a keyword candidates list and a scorer to rank these keywords.
/// - `sentence_embeddings_model`: Sentence embeddings model
/// - `tokenizer`: tokenizer used to generate the list of candidates (differs from the transformer tokenizer)
pub struct KeywordExtractionModel<'a> {
    pub sentence_embeddings_model: SentenceEmbeddingsModel,
    pub tokenizer: StopWordsTokenizer<'a>,
    scorer_type: KeywordScorerType,
    ngram_range: (usize, usize),
    num_keywords: usize,
    diversity: Option<f64>,
    max_sum_candidates: Option<usize>,
}

impl<'a> KeywordExtractionModel<'a> {
    /// Build a new `KeywordExtractionModel`
    ///
    /// # Arguments
    ///
    /// * `config` - `KeywordExtractionConfig` object containing a sentence embeddings configuration and tokenizer-specific options
    ///
    /// # Example
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use rust_bert::pipelines::keywords_extraction::KeywordExtractionModel;
    ///
    /// let keyword_extraction_model = KeywordExtractionModel::new(Default::default())?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(
        config: KeywordExtractionConfig<'a>,
    ) -> Result<KeywordExtractionModel<'a>, RustBertError> {
        let tokenizer_config = SentenceEmbeddingsTokenizerConfig::from_file(
            &config
                .sentence_embeddings_config
                .tokenizer_config_resource
                .get_local_path()?,
        );
        let sentence_bert_config = SentenceEmbeddingsSentenceBertConfig::from_file(
            &config
                .sentence_embeddings_config
                .sentence_bert_config_resource
                .get_local_path()?,
        );
        let sentence_embeddings_model =
            SentenceEmbeddingsModel::new(config.sentence_embeddings_config)?;

        let do_lower_case = tokenizer_config
            .do_lower_case
            .unwrap_or(sentence_bert_config.do_lower_case);

        let tokenizer = StopWordsTokenizer::new(
            config.tokenizer_stopwords,
            config.tokenizer_pattern,
            do_lower_case,
        );
        Ok(Self {
            sentence_embeddings_model,
            tokenizer,
            scorer_type: config.scorer_type,
            ngram_range: config.ngram_range,
            num_keywords: config.num_keywords,
            diversity: config.diversity,
            max_sum_candidates: config.max_sum_candidates,
        })
    }

    /// Extract keywords from a list of input texts.
    ///
    /// # Arguments
    ///
    /// * `inputs` - slice of string-like input texts to extract keywords from
    ///
    /// # Returns
    ///
    /// * `Result<Vec<Vec<Keyword>>, RustBertError>` containing a list of keyword for each input text
    ///
    /// # Example
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use rust_bert::pipelines::keywords_extraction::KeywordExtractionModel;
    ///
    /// let keyword_extraction_model = KeywordExtractionModel::new(Default::default())?;
    /// let input = [
    ///     "This is a first sentence to extract keywords from.",
    ///     "Some keywords will be extracted from this text too.",
    /// ];
    /// let output = keyword_extraction_model.predict(&input);
    /// # Ok(())
    /// # }
    /// ```
    pub fn predict<S>(&self, inputs: &[S]) -> Result<Vec<Vec<Keyword>>, RustBertError>
    where
        S: AsRef<str> + Sync,
    {
        let words = self.tokenizer.tokenize_list(inputs, self.ngram_range);
        let (flat_word_list, document_boundaries) =
            KeywordExtractionModel::flatten_word_list(&words);

        let document_embeddings = self
            .sentence_embeddings_model
            .encode_as_tensor(inputs)?
            .embeddings;

        let word_embeddings = self
            .sentence_embeddings_model
            .encode_as_tensor(&flat_word_list)?;

        let mut output_keywords: Vec<Vec<Keyword>> = Vec::new();
        for (document_index, (start, end)) in document_boundaries.into_iter().enumerate() {
            let mut document_keywords = Vec::new();
            let document_embedding = document_embeddings
                .select(0, document_index as i64)
                .unsqueeze(0);
            let word_embeddings = word_embeddings
                .embeddings
                .slice(0, start as i64, end as i64, 1);
            let num_keywords = min(self.num_keywords, word_embeddings.size()[0] as usize);
            let local_top_word_indices = self.scorer_type.score_keywords(
                document_embedding,
                word_embeddings,
                num_keywords,
                self.diversity,
                self.max_sum_candidates,
            );
            for (index, score) in local_top_word_indices {
                let word = flat_word_list[start + index];
                document_keywords.push(Keyword {
                    text: word.to_string(),
                    score,
                    offsets: words[document_index].get(word).unwrap().clone(),
                });
            }
            document_keywords.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
            output_keywords.push(document_keywords)
        }

        Ok(output_keywords)
    }

    fn flatten_word_list(
        words: &'a [HashMap<Cow<str>, Vec<Offset>>],
    ) -> (Vec<&'a Cow<'a, str>>, Vec<(usize, usize)>) {
        let mut flat_word_list = Vec::new();
        let mut doc_boundaries = Vec::with_capacity(words.len());
        let mut current_index = 0;
        for doc_words_map in words {
            let doc_words = doc_words_map.keys();
            let doc_words_len = doc_words_map.len();
            flat_word_list.extend(doc_words);
            doc_boundaries.push((current_index, current_index + doc_words_len));
            current_index += doc_words_len;
        }
        (flat_word_list, doc_boundaries)
    }
}
