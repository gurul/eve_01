# EmbeddingGemma vs text-embedding-3-small: the bar

Written 2026-10-09, before any arm was scored. Nothing below changes after the first run.

## Question

Can EmbeddingGemma (`embeddinggemma-300m`, run locally) replace OpenRouter's `text-embedding-3-small` as
the embedder of eve's mem0 memory, without losing recall and while answering faster?

## Data

- `corpus.jsonl`: 40 memory snippets about a fictional person.
- `questions.jsonl`: 30 questions, each with 1 or 2 correct snippet ids.
- Both files were written by a separate agent that had not seen eve's code or this bar. They hold no real
  person's data.

## Arms

- **openrouter**: `text-embedding-3-small`, 1536 dimensions, through OpenRouter, built the way
  `mem0_memory.py` builds it (mem0's `OpenAIEmbedding`, with its client replaced by `openrouter.client()`).
- **gemma**: EmbeddingGemma at 768 dimensions, local, built the way `mem0_memory.py` builds it with
  `EVE_MEM0_EMBEDDER=gemma` (mem0's `HuggingFaceEmbedding`, with eve's model-card task prompts:
  `task: search result | query: ` for a search, `title: none | text: ` for a stored memory).

Each arm embeds the 40 snippets as documents (`memory_action="add"`) and each question as a query
(`memory_action="search"`). The rank is cosine similarity over all 40 snippets. No threshold, no reranker.

## Metric

- **recall@5**: the number of questions (out of 30) with at least one correct snippet in the top 5.
- **query latency**: wall time of one `embed(question, "search")` call, per question, after one warm-up
  call that is not counted. Median and p95 over the 30 questions.
- Also reported, not gated: recall@1, MRR, cold-load time of the gemma model, peak RAM (max RSS) of the
  process, and install size (venv growth and model files on disk).

## Bar (both must hold for PASS)

1. gemma recall@5 >= openrouter recall@5 - 2 (questions).
2. gemma median query latency < openrouter median query latency.

Each arm is scored once. A failed bar is reported as failed; there is no second run with changed settings.
