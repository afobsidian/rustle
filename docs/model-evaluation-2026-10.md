# Local meeting notes model evaluation — October 2026

The previous provider benchmark uses a three-sentence transcript and a 30-second
timeout. That does not establish reliability or latency for an hour-long meeting.
Its Tokio timeout also cannot cancel an already-running blocking native inference
call. Candidate replays therefore run serially in separate processes.
The live Rustle service remains enabled: during the 9B replay it also recorded,
transcribed and summarised another meeting. All benchmark threads were lowered
to nice 10 to favour that workflow. The 9B timing includes contention and is not
a controlled speed comparison. Timings come from the local test harness and are
not a release-build performance guarantee.
This evaluation uses the same retained 772-segment, 45,859-character transcript
that triggered a repetitive-output failure. Private meeting text and generated
notes stay outside the repository.

Hardware: Intel i7-1185G7, four physical cores, eight threads, 32 GB RAM.
AC power with the existing performance profile.
Rustle's current llama.cpp build uses the CPU. A Quadro T500 is present, but this
evaluation does not assume CUDA acceleration.

## Candidates

| Model | Q4_K_M download | Reason to evaluate |
| --- | ---: | --- |
| Qwen2.5-3B-Instruct | Existing installation | Baseline; the original greedy sampler failed after 1,567 seconds. |
| Qwen3-4B-Instruct-2507 | 2.50 GB | Non-thinking model with native 262,144-token context; straightforward text-only integration. |
| Qwen3.5-2B | 1.28 GB | Smaller non-thinking candidate added after the 4B latency result. |
| Gemma 4 E2B IT | 3.11 GB | Different compact model, evaluated after the smaller Qwen produced unsupported facts. |
| Qwen3.5-9B | 5.68 GB | Larger accuracy candidate after confirming that latency is secondary. |
| Qwen3.5-4B | 2.74 GB | Newer hybrid attention architecture and native 262,144-token context; requires explicit non-thinking handling. |

Context capacity is a model specification, not a claim that Rustle currently uses
that entire window. The provider currently caps its prompt and output at 32,768
tokens. Comparisons must use the same transcript, output budget, CPU backend, and
quantisation, and must inspect decisions and action items against the transcript.

Qwen2.5's published generation configuration uses sampling with temperature 0.7,
top-p 0.8, top-k 20 and repetition penalty 1.05. The production code previously
used greedy decoding without a repetition penalty. A model replacement alone
does not validate a fix for that generation path. A full-transcript replay with
the corrected sampler was stopped after 900 seconds without a result. It
overlapped a native build, so its latency is not a fair model comparison and
its generation outcome remains unverified.

## Measured results

The practical target is a useful draft, not a perfect transcript or speaker list.
Minor omissions and uncertain attendance matter less than changing what was
agreed, inventing a causal explanation, or assigning a task to the wrong person.
These are qualitative checks against the retained transcript, not a numerical
accuracy score.

| Model and sampling | Full-transcript outcome |
| --- | --- |
| Qwen2.5 3B, original greedy | Repetitive-output failure after 1,567 seconds in production. |
| Qwen2.5 3B, corrected sampling | Stopped at 900 seconds; build contention makes latency inconclusive. |
| Qwen3 4B Instruct | Stopped at 903 seconds while evaluating the prompt; no quality result. |
| Qwen3.5 9B, non-thinking | Completed in 3,641.33 seconds (2,664.38 prompt evaluation), under live-workflow contention; format passed, but reversed an explicit agreement and misstated priorities. |
| Qwen3.5 4B, non-thinking | Initial trial stopped at 988 seconds. Revised-prompt replay completed in 1,522.53 active seconds (1,206.74 prompt evaluation); useful summary, but unsupported action ownership and attendance. |
| Gemma 4 E2B IT, non-thinking | Completed in 826.70 seconds (685.82 prompt evaluation); supported follow-ups, but proposals/attendance needed tighter instructions. |
| Qwen3.5 2B, non-thinking | Completed in 511.55 seconds (392.71 prompt evaluation); format passed; substantial unsupported claims. Not recommended for this transcript. |

The Qwen3.5 2B output contained unsupported causal claims, incorrect attendee
inferences, and missed explicit actions. This demonstrates why valid Markdown
and absence of repetition do not establish that the draft is useful. The rejected
output is retained locally, not published as recovered meeting notes.

Gemma 4 E2B has 2.3B effective parameters and 5.1B including embeddings, with a
128K native context. The trial uses Google’s non-thinking turn format and
sampling settings (temperature 1.0, top-p 0.95, top-k 64).

## Other models reviewed

- Qwen3.8: newer than Qwen3.5, but the published lineup starts at 27B.
  It is outside this compact, CPU-focused comparison.

- Gemma 4 E4B is another larger accuracy candidate, not measured here.
  Published general benchmarks cannot establish its meeting-note accuracy.
- Granite 4.2 3B: released August 2026; native 128K context, with thinking,
  non-thinking and low-effort modes. Its reasoning scores do not directly measure
  factual meeting summarisation.
- Ministral 3 3B Instruct: another small current instruction model; not measured
  in this focused comparison.
- Liquid LFM2.5 2.6B: an edge-oriented hybrid with a 128K context window.
  Its card describes it as an always-thinking model; Rustle currently constrains
  output directly to Markdown, so it needs a separate integration evaluation.

Published benchmark results and hardware fit determine the shortlist. They do
not establish a winner for meeting notes. Measured generation success and
content review are required before changing the default model.

## Recommendation and limits

Recommend Gemma 4 E2B IT Q4_K_M for this laptop, including the stated
accuracy-first preference. It produced the most useful draft among the completed
trials: its core agreement was preserved, while the larger 9B candidate reversed
that agreement and misstated priorities. This is a qualitative choice from one
meeting, not a claim that the smaller model is universally more accurate.
It is also the compact default for fresh configurations.

Accuracy takes priority over latency for this installation. The smaller Qwen
was faster but its factual errors make it unsuitable for this meeting. Gemma
completed reliably, but factual review still found proposals listed as decisions
and an unsupported attendee inference. Those draft errors still need review.
The earlier 4B trials were stopped for latency before this preference was known.
The completed Qwen3.5 4B replay produced useful topic coverage, but assigned
actions to speakers whose ownership was not established and included an absent
person in attendees. The Qwen3.5 9B replay also completed without repetition,
but reversed an explicit agreement, invented or misstated prioritisation decisions,
and inferred unsupported owners and attendees. Its greater size did not improve
this draft enough to recommend it over Gemma. That judgement concerns content;
its contended runtime is not used to rank accuracy or compare speed.

The prompt now explicitly separates proposals from decisions, preserves tentative
timing, and excludes absent or merely mentioned people from attendees. A real
Gemma regression test with named speakers, an absent person, an unapproved
proposal, an approved decision and a dated action passed in 21.94 seconds.
The revised-prompt Gemma full-transcript replay completed in 1,489.88 seconds,
including 1,209.74 seconds evaluating the prompt. It overlapped build checks and
is not used for latency comparison. Its format and repetition checks passed, but
it still misplaced a proposal under decisions, inferred an attendee from a
mention, and hardened a tentative deadline. These are draft-quality limitations,
not inference failures; changing what was agreed matters more than minor omissions.
Passing the short regression does
not establish full-meeting factual accuracy.

This is one noisy ASR transcript on one machine with one sampling seed, not a
blinded summarisation benchmark. Overlapping speech, ASR errors and unidentified
speakers limit what any notes model can recover. Unknown attendance and ownership
should remain unknown; names mentioned in conversation are not speaker labels.
Generated notes require review. Current defaults
change for fresh configurations; existing configurations keep their model.

## Sources and reproducibility

- [Qwen3.8 official model lineup](https://github.com/QwenLM/Qwen3.8)
- [Qwen2.5 generation configuration](https://huggingface.co/Qwen/Qwen2.5-3B-Instruct/blob/main/generation_config.json)
- [Qwen3-4B-Instruct-2507 model card](https://huggingface.co/Qwen/Qwen3-4B-Instruct-2507)
- [Qwen3.5-2B model card](https://huggingface.co/Qwen/Qwen3.5-2B)
- [Qwen3.5-4B model card](https://huggingface.co/Qwen/Qwen3.5-4B)
- [Qwen3.5 9B model card](https://huggingface.co/Qwen/Qwen3.5-9B)
- [Gemma 4 E4B model card](https://huggingface.co/google/gemma-4-E4B-it)
- [Gemma 4 E2B model card](https://huggingface.co/google/gemma-4-E2B-it)
- [Granite 4.2 3B model card](https://huggingface.co/ibm-granite/granite-4.2-3b)
- [Ministral 3 3B documentation](https://docs.mistral.ai/models/ministral-3-3b-25-12)
- [Liquid LFM2.5 2.6B model card](https://huggingface.co/LiquidAI/LFM2.5-2.6B)

Qwen3.5-4B was stopped after 988 seconds and Qwen3-4B-Instruct-2507 after
903 seconds, both with prompt evaluation still running. These initial trials are
incomplete latency trials, not generation failures or quality scores. The later
Qwen3.5 4B replay completed. The laptop suspended during that replay; reported
Rust `Instant` durations exclude suspended time and are active processing times,
not the overnight wall-clock interval.
The clear-speaker regression also passed with Qwen3.5 4B in 58.51 seconds,
despite that model’s unsupported inferences on the noisy full meeting.

The smaller 2B candidate uses its own published text sampling settings: temperature
1.0, top-p 1.0, top-k 20 and presence penalty 2.0.

Candidate GGUF files were downloaded from Unsloth at pinned revisions and
verified against their published LFS SHA-256 values:

| File | Revision | SHA-256 |
| --- | --- | --- |
| Qwen3-4B-Instruct-2507-Q4_K_M.gguf | `a06e946bb6b655725eafa393f4a9745d460374c9` | `3605803b982cb64aead44f6c1b2ae36e3acdb41d8e46c8a94c6533bc4c67e597` |
| Qwen3.5-2B-Q4_K_M.gguf | `f6d5376be1edb4d416d56da11e5397a961aca8ae` | `aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223` |
| gemma-4-E2B-it-Q4_K_M.gguf | `0314792d7f1f7e229411f620751375812bb9faf2` | `740185b21d22ceb83a11c3aa62ad5842ef32c70f6096d756bbee85a1e4ec34b8` |
| Qwen3.5-9B-Q4_K_M.gguf | `3885219b6810b007914f3a7950a8d1b469d598a5` | `03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8` |
| Qwen3.5-4B-Q4_K_M.gguf | `e87f176479d0855a907a41277aca2f8ee7a09523` | `00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4` |

The ignored `saved_transcript_produces_real_notes` provider test runs the actual
local model using `RUSTLE_TEST_MODEL`, `RUSTLE_TEST_TRANSCRIPT` and
`RUSTLE_TEST_NOTES` paths. It fails on repetitive output or missing note sections.
It does not independently verify the factual accuracy of the generated notes.
