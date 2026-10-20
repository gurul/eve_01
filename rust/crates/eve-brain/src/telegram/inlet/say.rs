//! A port of `telegram.py`, lines 3715-4117: sending, inline buttons, taps, progress messages.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;
#[allow(unused_imports)]
use crate::telegram::*;

impl TelegramInlet {
    /// py: telegram.py:3715 `async def _say(self, chat_id: int, text: str, title: Optional[str] = None, subtitle: Optional[str] = None, buttons: Sequence[str] = (), *, reply_to: int = 0, force_reply: Optional[str] = None) -> None:`
    ///
    /// Send one message. ``title`` names the voice or the event when it is not eve's own reply
    /// (telegram_format.compose); ``subtitle`` is an identifier that helps a person, or nothing;
    /// ``buttons`` are tap-to-send replies; ``reply_to`` is the owner's message this answers; ``force_reply``
    /// opens the reply box on it. A message Telegram refuses with a reply link or a reply box goes again
    /// without them: the words always matter more than the link.
    ///
    /// Work an Eve Link app turn began goes to the app instead (``_to_app``), and is not posted here; a free
    /// question (``force_reply``) goes as an "ask", which ends the app's stream for this turn so the owner's next
    /// text from the app can answer it. When the app is not reading, it comes here as before.
    pub(crate) async fn say(self: &Arc<Self>, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) {
        todo!("telegram.py:3715 TelegramInlet._say")
    }

    /// py: telegram.py:3768 `async def _screen_now(self, chat_id: int) -> None:`
    pub(crate) async fn screen_now(self: &Arc<Self>, chat_id: i64) {
        todo!("telegram.py:3768 TelegramInlet._screen_now")
    }

    /// py: telegram.py:3774 `def _register(self, board: _Keyboard) -> None:`
    ///
    /// Give each of a keyboard's choices a fresh short key. Old keys are forgotten past MAX_LIVE_BUTTONS,
    /// oldest first: a tap on one of those is "expired", as after a restart.
    pub(crate) fn register(self: &Arc<Self>, board: &Arc<_Keyboard>) {
        todo!("telegram.py:3774 TelegramInlet._register")
    }

    /// py: telegram.py:3786 `def _retire(self, board: Optional[_Keyboard], *, drop: bool = False) -> None:`
    ///
    /// Forget a keyboard's keys; with ``drop``, take its buttons off the message too (fail-soft).
    pub(crate) fn retire(self: &Arc<Self>, board: Option<&Arc<_Keyboard>>, drop: bool) {
        todo!("telegram.py:3786 TelegramInlet._retire")
    }

    /// py: telegram.py:3795 `async def _send_choices(self, chat_id: Optional[int], text: str, board: _Keyboard, *, title: Optional[str] = None, subtitle: Optional[str] = None, fallback: Sequence[str] = (), per_row: int = 1, read_all: bool = False) -> bool:`
    ///
    /// Send ``text`` with the keyboard's buttons under it → True. When Telegram (or a fake) will not take
    /// an inline keyboard, the keys are forgotten and the message goes as it did before buttons: plain,
    /// or with ``fallback`` as a reply keyboard. The message is never lost for want of its buttons.
    ///
    /// A question or a picker from work an Eve Link app turn began goes to the app as words with the replies
    /// that answer it (the app has no buttons: the owner types one), and True keeps the question strict, as a
    /// sent keyboard does. A question goes as an "ask", which ends the app's stream for the turn. A progress
    /// card (Stop) and the relay's choices stay in Telegram, where they are edited and tapped.
    pub(crate) async fn send_choices(self: &Arc<Self>, chat_id: Option<i64>, text: &str, board: &Arc<_Keyboard>, title: Option<&str>, subtitle: Option<&str>, fallback: &[String], per_row: i64, read_all: bool) -> bool {
        todo!("telegram.py:3795 TelegramInlet._send_choices")
    }

    /// py: telegram.py:3837 `async def _offer_picker(self, chat_id: int, text: str, choices: Sequence[Choice], *, title: Optional[str] = None, subtitle: Optional[str] = None, valid: Callable[[], bool] = lambda: True, fallback: Sequence[str] = ()) -> None:`
    ///
    /// A picker as inline buttons whose tap sends the button's words (SAY). Only the latest picker is
    /// live: a new one retires the last, whose buttons would now answer the wrong question.
    pub(crate) async fn offer_picker(self: &Arc<Self>, chat_id: i64, text: &str, choices: &[Choice], title: Option<&str>, subtitle: Option<&str>, valid: Option<BoolFn>, fallback: &[String]) {
        todo!("telegram.py:3837 TelegramInlet._offer_picker")
    }

    /// py: telegram.py:3847 `def _on_tap(self, tap: Tap) -> None:`
    ///
    /// One tap from the owner, decided here, synchronously, so two quick taps cannot both act. Every tap
    /// is answered (answerCallbackQuery), whatever it did. A key this run did not make, a keyboard already
    /// used, one for another chat or one whose moment has passed is answered and does nothing.
    pub(crate) fn on_tap(self: &Arc<Self>, tap: &Tap) {
        todo!("telegram.py:3847 TelegramInlet._on_tap")
    }

    /// py: telegram.py:3885 `async def _answer_tap(self, tap: Tap, text: str, *, drop: bool = False) -> None:`
    pub(crate) async fn answer_tap(self: &Arc<Self>, tap: &Tap, text: &str, drop: bool) {
        todo!("telegram.py:3885 TelegramInlet._answer_tap")
    }

    /// py: telegram.py:3895 `async def _drop_buttons(self, chat_id: int, message_id: int) -> None:`
    pub(crate) async fn drop_buttons(self: &Arc<Self>, chat_id: i64, message_id: i64) {
        todo!("telegram.py:3895 TelegramInlet._drop_buttons")
    }

    /// py: telegram.py:3903 `async def _settle_prompt(self, board: _Keyboard, body: str, line: str, title: Optional[str], subtitle: Optional[str]) -> None:`
    ///
    /// A decided prompt, rewritten to say what was decided (``body`` then ``line``), its buttons gone
    /// (fail-soft: the decision stands whether or not the message changes). An edit holds one piece, on the
    /// message the buttons sit on, which is the last piece of a long prompt: rewriting it with the prompt's
    /// start would duplicate that and cut the outcome (review, 2026-09-23). A prompt longer than one piece
    /// loses its buttons instead, and the outcome goes as a short message of its own.
    pub(crate) async fn settle_prompt(self: &Arc<Self>, board: &Arc<_Keyboard>, body: &str, line: &str, title: Option<&str>, subtitle: Option<&str>) {
        todo!("telegram.py:3903 TelegramInlet._settle_prompt")
    }

    /// py: telegram.py:3925 `def _retire_options(self) -> None:`
    ///
    /// The relayed question's option buttons stop meaning anything once the owner types to Claude, the
    /// turn ends, or the relay moves: a late tap would type a number into whatever comes next.
    pub(crate) fn retire_options(self: &Arc<Self>) {
        todo!("telegram.py:3925 TelegramInlet._retire_options")
    }

    /// py: telegram.py:3932 `def _open_progress(self, chat_id: int, head: str, on_stop: Callable[[int], None], *, title: Optional[str] = None, subtitle: Optional[str] = None, request_id: int = 0) -> _Progress:`
    ///
    /// Start a progress message for work that is starting now → its handle. It is sent at once (a job)
    /// with a Stop button (a STOP keyboard) whose tap calls ``on_stop``: it stops this work, as texting
    /// "stop" would (a task is cancelled, a Codex turn interrupted), and never some other work that the
    /// typed word would reach first. The button is live only while the work is: after the close, a tap is
    /// "expired".
    pub(crate) fn open_progress(self: &Arc<Self>, chat_id: i64, head: &str, on_stop: OnStop, title: Option<&str>, subtitle: Option<&str>, request_id: i64) -> Arc<_Progress> {
        todo!("telegram.py:3932 TelegramInlet._open_progress")
    }

    /// py: telegram.py:3946 `async def _send_progress(self, progress: _Progress) -> None:`
    pub(crate) async fn send_progress(self: &Arc<Self>, progress: &Arc<_Progress>) {
        todo!("telegram.py:3946 TelegramInlet._send_progress")
    }

    /// py: telegram.py:3964 `def _progress_step(self, progress: Optional[_Progress], line: str) -> None:`
    ///
    /// One step of the work (a task's narration, a Codex commentary), whole. It is added to the progress
    /// message by an edit, paced to one per PROGRESS_EDIT_SECS; the same step twice in a row is shown once.
    /// Nothing is cut: a step too long for the message goes as its own message, and near the limit the
    /// oldest steps scroll off, each one the phone never showed sent as its own message first.
    pub(crate) fn progress_step(self: &Arc<Self>, progress: Option<&Arc<_Progress>>, line: &str) {
        todo!("telegram.py:3964 TelegramInlet._progress_step")
    }

    /// py: telegram.py:3997 `def _progress_fits(progress: _Progress, extra: Sequence[str] = (), *, alone: bool = False) -> bool:`
    ///
    /// True when the progress message, with ``extra`` steps and its longest closing line, is one piece
    /// under the limit once composed as HTML (escaping makes it longer than the words). With ``alone``,
    /// the steps already there are left out: would ``extra`` fit even after they all scroll off?
    pub(crate) fn progress_fits(progress: &Arc<_Progress>, extra: &[String], alone: bool) -> bool {
        todo!("telegram.py:3997 TelegramInlet._progress_fits")
    }

    /// py: telegram.py:4006 `def _restore_stop(self, progress: Optional[_Progress]) -> None:`
    ///
    /// A Stop tap retires its key before the stop runs. When the stop then fails (Codex: "still starting",
    /// "nothing is running" before the turn has an id) the work goes on, so the button comes back under a
    /// fresh key and the next edit shows it; without this nothing on the message could stop it any more
    /// (review, 2026-09-23).
    pub(crate) fn restore_stop(self: &Arc<Self>, progress: Option<&Arc<_Progress>>) {
        todo!("telegram.py:4006 TelegramInlet._restore_stop")
    }

    /// py: telegram.py:4018 `def _stop_rows(self, progress: _Progress) -> Optional[list[list[tuple[str, str, str]]]]:`
    ///
    /// The Stop button's row while its key is live, else None (a tapped or retired button stays gone).
    pub(crate) fn stop_rows(self: &Arc<Self>, progress: &Arc<_Progress>) -> Option<Vec<Vec<(String, String, String)>>> {
        todo!("telegram.py:4018 TelegramInlet._stop_rows")
    }

    /// py: telegram.py:4025 `async def _flush_progress(self, progress: _Progress) -> None:`
    ///
    /// Put the latest steps on the phone, no sooner than PROGRESS_EDIT_SECS after the last change, and
    /// again while steps keep coming. An edit that would change nothing is skipped.
    pub(crate) async fn flush_progress(self: &Arc<Self>, progress: &Arc<_Progress>) {
        todo!("telegram.py:4025 TelegramInlet._flush_progress")
    }

    /// py: telegram.py:4050 `async def _push_progress(self, progress: _Progress, text: str) -> None:`
    ///
    /// Edit ``text`` into the progress message. On success only the steps taken with ``text`` count as
    /// shown: a step that arrives while the edit is in flight stays unsent, so a later failed edit or a
    /// failed close still sends it as a plain message rather than losing it (owner, 2026-09-23).
    pub(crate) async fn push_progress(self: &Arc<Self>, progress: &Arc<_Progress>, text: &str) {
        todo!("telegram.py:4050 TelegramInlet._push_progress")
    }

    /// py: telegram.py:4080 `async def _say_unsent(self, progress: _Progress) -> None:`
    ///
    /// The steps no edit has shown, as one plain message: today's behaviour when editing does not work.
    /// The long-step pointer is left out, since the step it points to already went on its own.
    pub(crate) async fn say_unsent(self: &Arc<Self>, progress: &Arc<_Progress>) {
        todo!("telegram.py:4080 TelegramInlet._say_unsent")
    }

    /// py: telegram.py:4088 `async def _close_progress(self, progress: Optional[_Progress], line: str) -> None:`
    ///
    /// The work is over: the progress message says ``line`` under its last steps and loses its Stop
    /// button. That closing edit shows every step, so none waits for the paced flush. When there is no
    /// message to edit (the buttons were refused) or the edit fails, the steps not yet shown go as one plain
    /// message before the result: a step is never lost, least of all a Codex commentary (owner, 2026-09-23).
    /// A flush waiting out its second is cancelled; one already sending steps is waited for, not cut off.
    /// Fail-soft throughout.
    pub(crate) async fn close_progress(self: &Arc<Self>, progress: Option<&Arc<_Progress>>, line: &str) {
        todo!("telegram.py:4088 TelegramInlet._close_progress")
    }
}
