# EmbeddingGemma vs text-embedding-3-small: the verdict

Run 2026-10-09 on the owner's Mac (Apple silicon), each arm once, against the bar in `BAR.md` (written
before either arm ran). Raw output: `results-gemma.json`, `results-openrouter.json`. Script: `run_eval.py`.

## Result: PASS

| | openrouter (`text-embedding-3-small`, 1536) | gemma (EmbeddingGemma, 768, local) |
|---|---|---|
| recall@5 (of 30) | 30 | **30** |
| recall@1 (of 30) | 28 | 28 |
| MRR | 0.967 | 0.967 |
| query latency, median | 550.6 ms | **19.7 ms** |
| query latency, p95 | 684.9 ms | 53.6 ms |
| building the embedder | 0.12 s | 3.06 s (model from the local cache, warm disk) |
| first call after building | 0.77 s | 0.23 s |
| embedding the 40 snippets | 1.18 s (one request) | 0.66 s |
| peak RAM of the process | 126 MB | 1035 MB |
| device | remote | `mps:0` (Apple GPU) |

- Bar 1, gemma recall@5 >= openrouter recall@5 - 2: 30 >= 28. **Holds.**
- Bar 2, gemma median query latency < openrouter's: 19.7 ms < 550.6 ms. **Holds.**

## Read it with these limits

- **The set is at its ceiling.** Both arms found every answer in the top 5 and missed the same two at rank 1
  (q23 and q29, both rank 2). 30 questions over 40 snippets cannot tell these two models apart on quality;
  it shows only that gemma is not clearly worse here. A harder or larger set is what would separate them.
- The latency of the openrouter arm is a network round trip from this Mac to OpenRouter on 2026-10-09; it
  varies with the network. Gemma's is the MPS device; on CPU it would be slower (not measured).
- Latency is the embedding call only. mem0's search also reads the Qdrant store; that part is the same for
  both arms and is not in these numbers.

## Install footprint

- Python: the `gemma` extra (`sentence-transformers` 6.1.0, which pulls `torch` 2.14.1 and `transformers`
  5.19.0) grew the venv from 242 MB to 1.0 GB, about **0.8 GB**.
- Model: `unsloth/embeddinggemma-300m` at revision `bfa3c846ac738e62aa61806ef9112d34acb1dc5a`, about
  **1.27 GB** of files in the Hugging Face cache (`model.safetensors` 1.21 GB, float32). The first download
  and load took 45 s.
- RAM: about **0.9 GB more** resident than the openrouter arm, for as long as the daemon holds the model.

## Where the model comes from

`google/embeddinggemma-300m` is gated on Hugging Face (`gated: manual`, a license acceptance per account),
and there was no Hugging Face token on this Mac. No license was accepted for the owner. The ungated copy
`unsloth/embeddinggemma-300m` was used: its JSON configs have the same git blob ids as Google's repo, and
its weight files have the same byte sizes (Google's LFS hashes are hidden behind the gate, so the weights
themselves could not be compared hash for hash). The Gemma Terms of Use apply to either copy.
The prompts come from the model's own `config_sentence_transformers.json`, and they match the model card:
`task: search result | query: ` for a query, `title: none | text: ` for a document.
