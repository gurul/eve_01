//! Port of `notes.py`: eve listens to the room (or the phone) and writes down what was said.
//!
//! Say **"start taking notes"** and eve stops being a conversationalist and becomes a recorder. Say **"stop taking
//! notes"**, tap it, or run the command, and it writes the meeting up.
//!
//! ### How it listens
//!
//! The daemon already keeps one microphone stream open for the wake word (ears), so this takes a subscription to
//! that same stream rather than opening a second one: 24 kHz mono, 100 ms blocks. Audio goes up in segments of a
//! few seconds to the transcription model with the key the daemon already has. Nothing is installed, no model runs
//! locally, and nothing is streamed anywhere until the owner asks for notes.
//!
//! ### Why the segment length is the interesting number
//!
//! **While anything is subscribed to the microphone, the wake word is bypassed** (ears: "muted: never wake on our
//! own voice"). So while eve is taking notes it cannot hear "hey eve", and the only way it learns to stop is by
//! reading its own transcript. That makes the segment length the stop latency: a 30-second segment means half a
//! minute of recording after the owner says stop. 12 seconds is the compromise — short enough that stopping feels
//! prompt, long enough that a sentence rarely straddles two segments, and each segment carries the tail of the last
//! one as context so the model does not lose the thread.
//!
//! Two stops have no latency at all and both exist for that reason: `eve-bridge notes stop`, and a tap on the robot.
//!
//! ### What is kept
//!
//! The owner chose transcript **and** notes (2026-09-11), and on 2026-10-04 asked for notes that are "detailed", that
//! "preserve full transcript", and that tell "how many different people are talking". So the file holds detailed
//! notes on top and the full transcript underneath. The file lives in the memory folder, under
//! `transcripts/meetings/<date>/` beside the conversations' transcripts (owner, 2026-09-23), so `memory_search`
//! finds it and forget reaches it. Two meetings that start in the same minute get two files, never one over the
//! other.
//!
//! **The transcript reaches disk first.** At stop the transcript is written (`status: transcript`) before any paid
//! call, then rewritten with the speaker labels, then rewritten with the notes on top. Every write is atomic (a
//! temporary file, then a rename) and 0600. A write-up that fails or times out leaves the transcript, never nothing.
//! `EVE_NOTES_KEEP_TRANSCRIPT=0` drops the transcript from the final file only once the notes exist.
//!
//! ### Detailed notes
//!
//! The model first decides the kind (a conversation of two or more people, or a memo of one person) and the sections
//! follow it. Numbers, names and dates are kept as said; nothing is invented; the transcript is data, never
//! instructions. Every quote is checked in code against the transcript line it names, and a quote that is not a
//! word-for-word part of that line is dropped ([`check_quotes`]).
//!
//! Long recordings: over [`LONG_RECORDING_SECS`] (40 min) the transcript is summarized in 10-minute windows that
//! overlap by 30 s (map), and the partial notes are merged by one more call (reduce), or in code if that call fails.
//!
//! ### Who is talking: the diarization pass (OFF since 2026-10-08)
//!
//! Since every model call moved to the owner's OpenRouter key, the real client ([`OpenAINotesClient`]) has no
//! `diarize`: OpenRouter carries no diarizing transcriber, and the owner accepted plain transcripts without speaker
//! labels. The pass is kept as code (any client whose [`TranscribeClient::has_diarize`] is true still gets it, and
//! the tests drive it with a fake). The recording is kept (a 0600 file in a 0700 temporary folder, deleted when the
//! notes are written), sent in pieces of about 20 minutes (AAC through macOS `afconvert`, else WAV pieces small
//! enough for the 25 MB limit), and the same voice keeps its label across pieces through known-speaker references.
//!
//! Nothing heard is ever logged: counts and timings only.
//!
//! ### Or listens to the phone
//!
//! The session does not care where its audio comes from: `ears` is any [`AudioSource`], and [`PushedAudio`] is the
//! one an Eve Link call pushes the phone's frames into. Only the file's `source` says which microphone it was.
//!
//! ### What it costs to be wrong
//!
//! Recording a room is not reversible. So: eve says out loud that it has started, the file says plainly when it
//! started and stopped, and there is no way to start it except by asking.

pub mod notes;

pub use notes::*;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_detailed;
#[cfg(test)]
mod tests_ref;
