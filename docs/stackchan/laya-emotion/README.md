# Laya for Eve's eyes and chirps: research and tuning

**Archived 2026-09-26.** The controller, its tests and the three tools named below (`emotion_eval.py`, `emotion_head_tune.py`, `emotion_confirm.py`) moved to `past-experiments/bridge/`; nothing in the product imported them. The commands below record how the results were produced; they no longer run from `bridge/`. The hashes in the results files name the old paths, as they were when measured.

**Live follow-up:** the owner subsequently requested expressions on the robot. [Live Laya expressions](../laya-expressions/README.md) implements an explicitly enabled experimental top-choice mode using the original checkpoint. The research failures and measurements below remain unchanged.

**Decision, 2026-09-21: fast enough to investigate, not accurate enough to enable.**
We trained real local weights in nine configurations. Scorer-only tuning did not
improve accuracy. Adapting the decision transformer improved the original test
slightly, but regressed on a fresh confirmation set and lost to a simple text
baseline. Both learned confidence gates abstain on every example. Nothing in
this experiment is connected to the daemon or flashed onto Eve.

## What the model actually does

The installed checkpoint is `laya-multilingual-mlx`: an mmBERT text encoder,
two decision-transformer layers, and a scorer applied at option-marker positions.
It receives a text description and named alternatives; it does not see camera
pixels, interpret raw sound, draw eyes, or synthesize audio. The local config has
a 1,024-token total limit, a 256-token question/option budget, and unfitted
temperatures of 1.0. Its `confidence` output is normalized entropy, not a measured
probability of correctness. These facts were checked in the installed
`laya_mlx/{model,agent,common}.py` and checkpoint config, not inferred from a demo.

The upstream [model source and limits](https://github.com/NandhaKishorM/laya/tree/573e5b62696ba441230cd6be71d593331b5d23af)
warn that specialization and domain calibration matter. Its
[training notebook](https://github.com/NandhaKishorM/laya/blob/573e5b62696ba441230cd6be71d593331b5d23af/notebooks/laya_finetune_typed_decisions_2xT4_kaggle.ipynb)
uses both a sampled policy-gradient objective and soft cross-entropy, adapting
encoder and heads on two CUDA GPUs. Our experiments use **supervised
cross-entropy on MLX**, not a reproduction of its RL procedure. We freeze the
encoder to make a bounded adaptation test with the checkpoint already on this
Mac. The [MLX port](https://github.com/mizorewww/laya-mlx) provides inference;
the training tools here explicitly implement the additional optimization.

The author's linked [arXiv:2510.01237](https://arxiv.org/abs/2510.01237) concerns
confidence-aware routing. It is not evidence that this Laya checkpoint recognizes
robot emotions. Likewise, an unrelated paper using the acronym RLCD for
contrastive distillation must not be treated as Laya's training specification.

## Research that changes the design

| Evidence | Implication for Eve | Limit |
|---|---|---|
| [Mishra et al., eye-region study](https://arxiv.org/html/2410.14337v1): robot emotions were recognized better from full faces than from cropped eyes in a Furhat study. | Test whether people can actually distinguish Eve's expressions; classifier accuracy alone is insufficient. | This study did **not** compare eyes with eyes-plus-head-plus-color, nor validate RoboEyes. The earlier personality-doc description has been corrected. |
| [Music-driven robot prosody and gesture](https://arxiv.org/abs/2001.05863), also found through [alphaXiv](https://www.alphaxiv.org/abs/2001.05863). | Coordinate a short sound with a visual change; evaluate the combined expression. | Different robot, audio, and participants; no universal pitch-to-emotion lookup follows. |
| [Blended robot sonification study](https://link.springer.com/article/10.1007/s12369-021-00788-4). | Sound contours and their relationship to movement deserve perceptual testing. | Evidence for particular designed sounds is not evidence for Eve's existing chirps. |
| [Guo et al., calibration](https://proceedings.mlr.press/v70/guo17a.html). | Fit temperature on separate data and report NLL, Brier score, and calibration error. | Temperature cannot repair incorrect rankings or guarantee calibration after a distribution shift. |
| [GoEmotions](https://aclanthology.org/2020.acl-main.372/). | Useful background for text-emotion vocabulary and annotator disagreement. | Classifying a Reddit writer's emotion is a different target from selecting a companion's response; it was not imported as robot-policy ground truth. |
| [SetFit, discovered through alphaXiv](https://www.alphaxiv.org/abs/2209.11055), checked against the [authors' paper abstract](https://arxiv.org/abs/2209.11055). | A contrastively adapted sentence encoder plus a classifier is a worthwhile few-shot comparison when labels are scarce. | SetFit is not a drop-in Laya recipe and was not trained in this experiment. Its reported gains do not predict Eve results. |

The target is **Eve's appropriate expression**, not a claim about what the
person truly feels. A person mentioning grief should not make the robot proclaim
itself lonely. A quoted crash should not cause a physical startle. Raw microphone
and camera interpretation belong upstream, with actor, timestamp, source, and
uncertainty preserved when observations become text.

## Experiment fixed before its first run

One fixed `choice` question uses six existing expression names:
`calm`, `happy`, `curious`, `affection`, `surprised`, `startled`.
The option schema is hashed into each artifact. Boredom and loneliness remain
the firmware's drive states rather than reactions copied from a person's words.

The [scenario file](../../../bridge/tests/fixtures/emotion/scenarios.json) has
216 **assistant-authored synthetic** examples: 96 training, 36 development,
36 calibration, 48 test. Each pair of paraphrases stays in its scenario family
and split. The same assistant wrote both the examples and implementation;
disjoint text is not independent human annotation. No personal recordings or
private transcripts were used. There are only eight training families per class.

- Scorer study: 592,897 trainable parameters; learning rates `1e-5`, `5e-5`,
  `2e-4`, crossed with weight decay `0.01`, `0.1`; 24 epochs, batch 16.
- Decision-head study: 14,768,641 trainable parameters in the two decision
  layers and scorer; learning rates `1e-5`, `3e-5`, `1e-4`, weight decay `0.01`;
  12 epochs, batch 8. Encoder and type embedding remain frozen.
- Both use AdamW, gradient norm clipping at 1, seed `20260921`, and development
  macro-F1 with NLL as tie-break to choose the epoch and configuration.
- Temperature minimizes calibration NLL over a fixed logarithmic grid.
  The confidence threshold maximizes calibration coverage while allowing at
  most 5% errors with at least eight accepted examples. If impossible, `1.01`
  means abstain-all. This small-sample rule is experimental, not a statistical
  assurance of 95% deployment precision.
- A train-only unigram TF-IDF class-centroid baseline checks whether using a
  large model provides value. Its accuracy/F1 are meaningful; its one-hot
  outputs are not a calibrated probability baseline.

The decision-head experiment followed the failed scorer experiment. Its original
test is therefore reported as **exploratory**. After selecting its weights, we
wrote and sealed [36 new confirmation scenarios](../../../bridge/tests/fixtures/emotion/confirmation.json),
scored once, and did not tune further. They remain synthetic, English-only, and
authored with knowledge of the task; they are not a real-world generalization test.

## Measured results

| Model | Original test accuracy (48) | Original macro-F1 | Fresh confirmation accuracy (36) | Fresh macro-F1 |
|---|---:|---:|---:|---:|
| Untouched Laya scorer | 31/48 = 64.6% | 0.632 | 24/36 = 66.7% | 0.649 |
| Tuned scorer | 31/48 = 64.6% | 0.632 | Not run | Not run |
| Tuned decision transformer + scorer | 33/48 = 68.8% | 0.680 | 22/36 = 61.1% | 0.604 |
| TF-IDF centroid | 33/48 = 68.8% | 0.662 | 30/36 = 83.3% | 0.817 |

The selected scorer settings were `lr=1e-5`, decay `0.1`, epoch 24;
the selected decision-head settings were `lr=1e-4`, decay `0.01`, epoch 5.
The decision-head model's development accuracy was 80.6%, but calibration
accuracy was only 58.3%. Its fitted temperature was 3.458. On fresh confirmation,
head NLL was 1.007 versus baseline 0.925, and ECE was 0.184 versus 0.158.
Neither tuning experiment found an acceptable confidence threshold: coverage
is **0%, with precision undefined**, not 100% precision.

Warm, batch-one decision-head inference on this **Apple M5, 32 GB Mac** measured
5.98 ms median and 6.12 ms p95 over 48 test inputs. That includes text preparation,
the encoder, decision head, scorer, and probability calculation. It excludes
transcription, scene understanding, queues, transport, and drawing/sound latency.
Thus these measurements establish computational feasibility, not end-to-end
real-time behavior. Loading the saved weights and re-running inputs checks that
training-cache predictions agree with the actual runtime.

Full logits/probabilities, confusion matrices, trial selection, timings,
checksums, and environment information are in [scorer results](results.json),
[head results](head-results.json), and [confirmation results](confirmation-results.json).
Large weights live locally under `.test-artifacts/laya-emotion/` and
`.test-artifacts/laya-emotion-head/`; the original checkpoint is unmodified.

## How this would reach the eyes and speaker

The offline [controller](../../../past-experiments/bridge/src/eve_bridge/emotion_policy.py)
tests a 1.2-second minimum dwell, a four-second event lifetime, duplicate and
out-of-order rejection, mute, and an eight-second chirp cooldown. These timings
are engineering starting values, not values established by the papers.

| Selected expression | Existing visual vocabulary | Existing chirp vocabulary |
|---|---|---|
| calm | ordinary open eyes | none |
| happy | smiling lower lids | warble |
| curious | curious asymmetry and gaze | two rising notes |
| affection | warm smile/curiosity | soft warble |
| surprised | wide eyes | high blip |
| startled | wide eyes with flicker | double blip; requires a fresh physical event |

This is a mapping of vocabulary, **not a new wire command**. The present
`emote` command applies bounded valence/arousal deltas, not an exact expression
label; repeatedly sending deltas would accumulate mood. It also cannot override
the conversation face. In `main.cpp`, mood expression requires explore mode,
`AG_IDLE`, and no listening; in `eyes.cpp`, `AG_SPEAKING` selects `HAPPY`.

Before live use, introduce a separate expiring expression overlay with an event
id and explicit authority. Keep sleep/attention and listening ownership; use
speech-phase gaze and timing while allowing a validated expression to modulate
the eyes. Let one firmware sound scheduler arbitrate caption sounds, expression
chirps, mute, and the microphone listening period. Emit on new semantic events,
not on every partial transcript or model frame. Retain immediate physical
reflexes on the board. Test disconnect recovery and stale-event expiry on hardware.
The current controller deliberately leaves all agent phases authoritative;
the overlay and daemon integration have **not** been implemented or claimed tested.

## Reproduce and inspect

From the repository root, using the existing bridge environment with `[laya]`:

```sh
# Fresh output directories prevent overwriting a sealed experiment.
bridge/.venv/bin/python bridge/tools/emotion_eval.py run --artifact .test-artifacts/laya-emotion-new --report /tmp/scorer-new.json
bridge/.venv/bin/python bridge/tools/emotion_head_tune.py run --artifact .test-artifacts/laya-emotion-head-new --report /tmp/head-new.json

# Recalculate reported scores and repeat real inference with the saved artifacts.
bridge/.venv/bin/python bridge/tools/emotion_eval.py verify
bridge/.venv/bin/python bridge/tools/emotion_head_tune.py verify
bridge/.venv/bin/python bridge/tools/emotion_confirm.py verify

# Inspect a proposal. This does not make Eve move or play sound.
bridge/.venv/bin/python bridge/tools/emotion_head_tune.py predict --text 'I solved the puzzle!'
```

Artifacts are tied to the base-weight checksum and fixed option schema. Do not
reuse these weights for the desktop fast lane, a different label order, or other
typed questions. Training depends on internal APIs of `laya-mlx 0.1.0` and
`mlx 0.32.2`; review compatibility before upgrading.

## What the evidence supports next

This is a negative tuning result, not a claim that all Laya adaptation will fail.
The frozen encoder, tiny authored dataset, and application-specific labels limit
what it establishes. Do not widen a hyperparameter search on these same test sets
or lower the confidence requirement to manufacture deployable results.

The next useful input is representative Eve interaction sequences labeled for
**appropriate robot response**, including ambiguous/no-reaction cases and actor
attribution. Use independent reviewers, retain disagreement as soft labels, and
split by conversation/session/person. Compare Laya encoder adaptation or adapters
with a SetFit-style classifier and the simple baseline on a new untouched test.
Then evaluate short eye-only, sound-only, and combined clips with people, measuring
recognition/confusions, appropriateness, annoyance, and task interruption. Those
are the missing observations needed to justify a live integration.
