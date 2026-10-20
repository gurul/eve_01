Two tiny sentencepiece models (identity normalisation, `<blk>` user-defined) trained with the Python
`sentencepiece` 0.2 wheel the daemon uses, so `src/tests.rs` can check the Rust encoder's BPE and unigram
paths against the C++ library's output without the KWS model.
