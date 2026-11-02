//! The Keel window: state and behavior here, rendering split by region.

mod composer_view;
#[cfg(debug_assertions)]
mod dev;
mod keyboard;
mod main_pane;
mod prompt_suggestion;
mod question;
mod settings;
mod gitlab;
mod sidebar;
mod terminal_panel;
mod timer_pill;
mod dotify;
mod dotify_ui;
mod mr_chip;
mod browser_panel;
mod files_panel;
mod session_card;
mod session_summary;
mod tooltip;
mod model_picker;
mod projects;

/// A receipt line for the transcript, drawn like the card's own receipt.
pub(crate) fn receipt_line_el(line: &crate::approval::ReceiptLine, t: Tokens) -> gpui::Div {
    question::receipt_content(line, t, 1.0)
}

/// A receipt's arguments, read-only, as the card showed them.
pub(crate) fn receipt_args_el(args: &[(String, String)], t: Tokens) -> gpui::Div {
    question::args_block(args, t, None)
}

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use crate::style::zpx;

use gpui::{
    AppContext, Context, Entity, FocusHandle, KeyDownEvent, PathPromptOptions, SharedString, Subscription,
    Task, Window,
};
use gpui_base::input::{InputEditorStyle, InputEvent, InputState, TextareaState};
use keel_proto::{AgentEvent, ChatStatus, RpcStreamItem, SubscribeEvent, TurnStatus};
use keel_rpc::RpcClient;
use keel_theme::{Tokens, os_reduced_motion};

use crate::composer::{HarnessOption, ModelList, PrimaryButtonAction, QuestionPanelState, compute_button_action};
use crate::markdown::{ImageRef, MessageBlockState, TranscriptBlock};
use crate::state::{AppState, ChatItem};
use crate::style::c;
use crate::transcript::TranscriptState;

/// One message of the active chat: role plus its incremental block parse.
struct MsgEntry {
    role: String,
    /// The whole text (all text parts). A user or error message renders
    /// from it; for an assistant message it feeds file and MR detection.
    blocks: MessageBlockState,
    /// An assistant message's thinking, tool calls and text, in stream order.
    steps: Vec<crate::turn::Step>,
    /// Images on a user message, shown above its text.
    images: Vec<ImageRef>,
}

/// The transcript row under the mouse, set as rows paint. `outside` holds
/// from the mouse leaving the window to its next move: gpui keeps the last
/// position, so a redraw would otherwise find the row hovered again.
#[derive(Default)]
struct RowHover {
    row: std::cell::Cell<Option<usize>>,
    outside: std::cell::Cell<bool>,
}

/// Most images one message carries.
const MAX_IMAGES: usize = 10;

/// An image attached in the composer, stored on the host, not yet sent.
#[derive(Clone)]
struct PendingImage {
    attachment: keel_proto::Attachment,
    /// Local file for the composer thumbnail.
    path: std::path::PathBuf,
}

pub struct KeelWindow {
    pub state: AppState,
    pub client: Option<RpcClient>,
    pub transcript: TranscriptState,

    // Active chat: per-chat transcript state, cleared on switch.
    current_chat: Option<String>,
    chat_gen: u64,
    /// Opening a chat with history: `Some(None)` holds the transcript hidden
    /// while its rows arrive and the list lands on the end; `Some(Some(at))`
    /// fades it in from `at`; `None` shows it as is.
    chat_reveal: Option<Option<std::time::Instant>>,
    chat_stream: Option<String>,
    /// Session events that arrived before the `subscribe_chat` response told
    /// us which stream id is ours. Keyed by stream id.
    pending_events: HashMap<String, Vec<SubscribeEvent>>,
    messages: HashMap<String, MsgEntry>,
    /// Message ids in arrival order (the transcript's order), for grouping turns.
    msg_order: Vec<String>,
    dirty: BTreeSet<String>,
    parse_in_flight: bool,
    /// Each turn's file chips (by head message id) and the key they were
    /// computed under. Recomputed only when a tool finishes, a turn ends, or
    /// the chat loads: finding them scans the whole turn and stats files.
    turn_chips: HashMap<String, (TurnChipKey, Option<TranscriptBlock>)>,
    /// The current chat's message views, rebuilt from the engine's push frames.
    doc_views: HashMap<String, serde_json::Value>,
    /// Bumped when a turn ends, so every turn's chips get a fresh look.
    turn_gen: u64,
    /// Messages whose rows wait for the next batched publish (reasoning
    /// deltas), and the pending publish.
    publish_queue: Vec<String>,
    publish_task: Option<gpui::Task<()>>,
    /// A tool started since the last live text/thinking delta: the next
    /// delta starts a new step instead of extending the previous one.
    step_break: bool,
    /// First message typed before any chat existed; sent once the new chat is active.
    pending_first_message: Option<(String, String)>,

    // Composer, questions, rename.
    composer: Entity<TextareaState>,
    draft_save: Option<Task<()>>,
    prompt_suggestion: Option<String>,
    suggestion_revision: u64,
    suggestion_key: Option<u64>,
    suggestion_task: Option<Task<()>>,
    composer_placeholder: String,
    chat_summaries: HashMap<String, session_summary::ChatSummary>,
    pub(super) summary_hover: session_summary::SessionSummaryHover,
    question_panels: HashMap<String, QuestionPanelState>,
    question_input: Entity<TextareaState>,
    focus_question: bool,
    /// A card folding into its receipt after an answer, stop or cancel.
    question_exit: Option<question::QuestionExit>,
    /// The live card's height, measured each frame, for the exit fold.
    card_height: std::rc::Rc<std::cell::Cell<f32>>,
    /// Chats whose waiting card was stopped; the slot stays the composer's
    /// until the turn ends, even while the status still says waiting.
    question_stopped: std::collections::HashSet<String>,
    /// MR cards' Merge buttons that are armed, merging or failed, by row id.
    mr_merges: HashMap<String, crate::transcript::MergeUi>,
    /// Risky Run buttons clicked once, by row id.
    armed_runs: std::collections::HashSet<String>,
    // Subagents of the active chat: engine `subagents.{id}` views by id,
    // the one whose session replaces the transcript, and what the harness
    // shows of them.
    subagents: BTreeMap<String, serde_json::Value>,
    subagent_open: Option<String>,
    subagent_caps: keel_proto::SubagentCaps,
    show_subagent_menu: bool,
    /// The points menu above the composer.
    show_points_menu: bool,
    /// Dev demo (`KEEL_TEST_CARD` / `KEEL_DEMO_PERMISSION`): the folded
    /// receipt stays in the slot instead of giving way to the composer.
    demo_hold_exit: bool,
    rename_input: Entity<InputState>,
    focus_handle: FocusHandle,
    input_style_for: Option<Tokens>,

    // Pickers.
    harnesses: Vec<HarnessOption>,
    models: HashMap<String, ModelList>,
    models_loading: HashSet<String>,
    /// When a harness's models arrived after its skeleton showed: the model
    /// section crossfades and glides from that instant.
    models_ready_at: HashMap<String, std::time::Instant>,
    /// Dev demo (`KEEL_DEMO_MODEL_LOADER`): the model picker opens and holds
    /// its loading skeleton, for a window screenshot.
    demo_model_loader: bool,
    models_error: HashMap<String, String>,
    selected_harness: String,
    selected_model: String,
    selected_effort: String,
    /// Sticks across new chats; sent as Ask where the harness lacks it.
    selected_permission: keel_proto::PermissionMode,
    show_permission_menu: bool,
    /// The chip's mode before the latest pick, for its roll (`permission_roll` > 0).
    permission_prev: keel_proto::PermissionMode,
    permission_roll: u64,
    model_chip_w: composer_view::ChipWidth,
    permission_chip_w: composer_view::ChipWidth,
    show_harness_picker: bool,
    show_model_picker: bool,
    show_effort_picker: bool,
    /// The thinking-effort dropdown is open, and where the last change came
    /// from (a 0..1 fraction of the levels, for the roll's direction) on the
    /// change numbered `effort_glide`.
    show_effort_menu: bool,
    effort_from: f32,
    effort_glide: u64,
    /// The level before the latest change, for the name's roll.
    effort_prev: String,
    /// The model chip's label roll: the label before the latest change, the
    /// roll's number, and whether it rolls up (later in the list) or down.
    model_prev: String,
    model_roll: u64,
    model_roll_up: bool,
    /// The hero branch chip's label width, easing between labels.
    branch_chip_w: Option<crate::motion::WidthTween>,
    /// The composer input's height, easing as lines come and go; its
    /// measured natural height (0 until measured); hero or docked when set.
    composer_h: Option<crate::motion::WidthTween>,
    /// Send button fill, 0 idle to 1 active. Unset until first painted.
    send_fill: Option<crate::motion::FillTween>,
    composer_natural: std::rc::Rc<std::cell::Cell<f32>>,
    composer_h_hero: bool,
    /// One line of composer text, tall: the least input height measured.
    composer_line_h: f32,
    /// The docked composer's layout at its width (inline or stacked, chips
    /// compacted), the panel's measured width, and the controls cluster's
    /// width as last measured at each compaction level.
    composer_fit: composer_view::ComposerFit,
    composer_fit_known: bool,
    /// When the composer last moved between inline and stacked.
    composer_fit_at: Option<std::time::Instant>,
    composer_panel_w: std::rc::Rc<std::cell::Cell<f32>>,
    composer_ctrl_w: std::rc::Rc<std::cell::RefCell<[f32; 3]>>,
    /// Its label now and before the last change, with their widths, and
    /// the change count that names the roll animation.
    branch_label: crate::motion::RollingLabel,
    branch_label_w: f32,
    branch_prev_w: f32,

    pub(super) os_reduced_motion: bool,
    /// A drag on the sidebar's edge is in progress.
    resizing_sidebar: bool,
    /// Sidebar motion: last frame's y per item (chat id, `h:`/`m:` + space
    /// id for headers and "Show more"), and the filter it was computed
    /// under; each moved item's start offset, and each newly shown item's
    /// place in its entering batch, with the generation naming the animation.
    row_order: HashMap<String, f32>,
    row_order_filter: Option<Option<String>>,
    /// Each entry also notes when it was planned: it is dropped once played,
    /// or a row re-wrapped later (after the open cascade) would replay it.
    /// A moved item's glide: its start offset (px from its new slot) and
    /// start time; drawn as a plain `top` offset, never a wrapper.
    row_glide: HashMap<String, (f32, std::time::Instant)>,
    row_enter: HashMap<String, (usize, u64, std::time::Instant)>,
    /// Per space: the generation of its last fold toggle, for the chevron's turn.
    space_fold_gen: HashMap<String, u64>,
    glide_gen: u64,
    /// The floating "Latest" pill's enter/exit.
    latest_pill: crate::motion::Presence,
    /// Sent, and nothing visible has come back yet: show the thinking row.
    awaiting_first_output: bool,
    /// What the loader says: verbs for the current phase, and when and with
    /// which seed this turn started (so verbs rotate, and differ per turn).
    loader_verbs: &'static [&'static str],
    loader_seed: (std::time::Instant, usize),
    /// Transcript row the inline loader was attached to last frame, so the
    /// rows it leaves and joins get remeasured.
    loader_row: Option<usize>,
    /// When the inline loader began its entrance, and (row, when) it began
    /// leaving after the run ended.
    loader_at: Option<std::time::Instant>,
    /// The next publish animates its rows even if the chat is idle (a fold the user toggled).
    row_motion_once: bool,
    loader_exit: Option<(usize, std::time::Instant)>,
    /// Rows at the last render: the row that was last keeps its tail room
    /// cached until remeasured.
    tail_len: usize,
    /// Last sidebar open/close, so the width can animate. The counter names
    /// the animation so each toggle replays it.
    sidebar_anim: Option<(std::time::Instant, u64)>,
    /// The instant this frame samples the sidebar and right panel slides at.
    /// One clock per frame: the sidebar's width, the pane's left edge and the
    /// reading column (and its composer) all read the same progress, so a
    /// box laid out from two of them can't land a few px apart mid-slide.
    frame_now: std::cell::Cell<Option<std::time::Instant>>,
    /// How open the sidebar was when the current toggle began (0 shut, 1
    /// open): a toggle mid-slide turns around from where the panel is.
    sidebar_from: f32,
    /// Last right sidepanel open/close (browser, files, terminal), so the chat width animates smoothly.
    right_panel_anim: Option<(std::time::Instant, u64)>,
    /// How open the right sidepanel was when the current toggle began (0 shut, 1 open).
    right_panel_from: f32,
    /// The sidebar or right panel slide in progress: the reading column's
    /// start and landing geometry. The transcript and composer both read it.
    column_slide: Option<session_card::ColumnSlide>,
    /// The reading column as last rendered, where the next slide starts.
    column_last: session_card::ColumnGeo,
    /// Debug: the per-frame layout trace `ui-dev.sock`'s `trace` reads.
    geo_trace: std::rc::Rc<dev::GeoTrace>,
    /// Named groups of folders (`{data_dir}/projects.json`).
    projects: projects::Projects,
    /// New-chat hero: shown last frame, when it last went away (for the
    /// composer's drop to the bottom), and a counter naming its animations.
    was_hero: bool,
    /// Hero → chat exit progress (0..1) and last tick. Advances at most one
    /// 60 Hz frame per paint (same idea as `page_at`), so a slow first
    /// history build delays the glide rather than skipping half of it.
    hero_exit: Option<(f32, std::time::Instant)>,
    /// `hero_image_path` looked up at most every couple of seconds, not
    /// with stat() calls on every frame.
    hero_path: std::cell::RefCell<Option<(std::time::Instant, Option<std::path::PathBuf>)>>,
    hero_gen: u64,
    /// Measured, not guessed: where the hero composer panel was last painted,
    /// where the docked composer's slot is, and the hero position captured
    /// when the hero went away (the drop starts exactly there).
    hero_panel: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    model_chip_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    effort_chip_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    permission_chip_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    card_branch_bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// The docked composer's height: it floats over the transcript's foot.
    dock_h: std::rc::Rc<std::cell::Cell<f32>>,
    /// `dock_h` as the newest row was last measured with.
    tail_dock_h: f32,
    /// Where the hero title was last painted, so it can fade out in place.
    hero_title: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    dock_slot: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    hero_from: Option<gpui::Bounds<gpui::Pixels>>,
    /// A first message was sent from the hero and its chat is being created:
    /// the hero leaves now instead of after the round trip.
    creating_chat: bool,
    /// Collapsed sidebar peeking as a floating panel (hover on the toggle),
    /// the pending close once the pointer leaves, and a counter naming the
    /// slide-in so each peek replays it.
    sidebar_peek: bool,
    /// `KEEL_FRAME_LOG`: frame timing during sidebar toggles.
    frame_probe: crate::frame_probe::FrameProbe,
    /// Sliding back out: still drawn until its exit finishes.
    peek_closing: bool,
    peek_close: Option<Task<()>>,
    peek_gen: u64,
    /// Settings page (⌘,): open, the agents' status as last checked, the
    /// connected services, and the GitLab connect form.
    settings_open: bool,
    last_settings_toggle: Option<std::time::Instant>,
    /// Still painted after close so the page can leave. The number names
    /// the exit; a newer open or close wins.
    settings_leaving: Option<u64>,
    settings_gen: u64,
    /// The page on screen (0 chat, 1 Settings, 2 GitLab), the one before,
    /// and when it changed: the new page glides in from its side.
    page_key: u8,
    page_from: u8,
    /// (progress 0..1, last frame).
    page_at: Option<(f32, std::time::Instant)>,
    harness_status: Option<Vec<settings::HarnessStatus>>,
    /// Settings → Accounts: the sign-in in progress (provider id and its
    /// last status), and the provider that just landed, whose "Connected"
    /// fades in once (the number names that entrance).
    sign_in: Option<(String, settings::SignInUi)>,
    auth_landed: Option<(String, u64)>,
    connections: Vec<settings::ConnectionRow>,
    gitlab_host: Entity<InputState>,
    /// Settings → Browser: the allowed-sites list, and whether it has been
    /// filled from the saved list since Settings opened.
    browser_sites: Entity<InputState>,
    browser_sites_synced: bool,
    gitlab_token: Entity<InputState>,
    /// Dotify: the timer pill's work clock (see `dotify.rs`).
    dotify: dotify::DotifyUi,
    dotify_token: Entity<InputState>,
    dotify_search: Entity<InputState>,
    dotify_desc: Entity<TextareaState>,
    connect_busy: bool,
    connect_error: Option<String>,
    /// Switches flipped lately, for their slide: switch id → (flip number, when).
    switch_flips: HashMap<String, (u64, std::time::Instant)>,
    switch_gen: u64,
    /// GitLab page (sidebar): open, what it shows, and its project filter.
    gitlab_open: bool,
    gitlab: gitlab::GitlabPage,
    gitlab_filter: Entity<InputState>,
    /// The `@` menu while a reference is being typed: where its `@` is, the
    /// text after it, and the highlighted row. Paths are fetched once per
    /// folder and kept.
    mention: Option<Mention>,
    /// The menu as last drawn, to see it open or close; the counter that
    /// replays its rows' cascade per opening; and a closed menu's snapshot
    /// fading out (with its exit number, from its own counter).
    mention_shown: Option<Mention>,
    mention_gen: u64,
    mention_leaving: Option<(u64, Mention)>,
    mention_leave_gen: u64,
    folder_paths: HashMap<String, Vec<String>>,
    /// Slash commands per harness and folder (`harness\0folder`), fetched
    /// once when `/` is first typed there.
    commands: HashMap<String, Vec<SlashCommand>>,
    commands_loading: HashSet<String>,
    folder_paths_loading: HashSet<String>,
    /// The zoom-level pill: which zoom change it's showing (names its fade),
    /// and the timer that removes it.
    /// The bool: it was already up, so this change shouldn't replay the entrance.
    zoom_toast: Option<(u64, bool)>,
    zoom_toast_task: Option<Task<()>>,
    /// Sidebar multi-select: chosen chats in click order, the row a ⇧-click
    /// range starts from, and the chats awaiting delete confirmation.
    pub(super) select_mode: bool,
    selected_chats: Vec<String>,
    select_anchor: Option<String>,
    confirming_delete_chats: Option<Vec<String>>,
    /// Replays the centered delete modal's entrance.
    delete_confirm_gen: u64,
    /// Exit draw: generation + the ids the modal still shows while fading out.
    delete_confirm_leaving: Option<(u64, Vec<String>)>,
    /// Entry replay for the select-mode checkmarks and bar.
    select_mode_gen: u64,
    /// Exit generation while the bar and checks fade out after Clear.
    select_leaving: Option<u64>,
    /// Rows playing their leave animation before the delete lands.
    rows_exiting: Vec<String>,
    rows_exit_gen: u64,
    /// Fullscreen hides the traffic lights, so nothing leaves room for them.
    fullscreen: bool,
    /// The GitLab project a new chat's issue lives in (optional): its host
    /// and project, the projects to pick from across connected hosts, and
    /// its open issues for the `@` menu. `None` lists = not loaded yet.
    issue_repo: Option<(String, gitlab::Project)>,
    repo_projects: Option<Result<Vec<(String, gitlab::Project)>, String>>,
    repo_projects_loading: bool,
    repo_issues: Option<Vec<gitlab::Issue>>,
    /// The hero pickers' shared search (folder, branch, repo: one menu is
    /// open at a time): the box, the query it settled on (applied a beat
    /// after typing stops), and the pending debounce.
    picker_filter: Entity<InputState>,
    picker_query: String,
    picker_filter_task: Option<Task<()>>,
    /// The model menu's highlight, folds and scroll.
    model_menu: model_picker::ModelMenu,
    /// Floating menus open last frame, and those fading out after a close
    /// (key, exit number) until the fade is done. See [`KeelWindow::menu_phase`].
    menus_open: HashSet<SharedString>,
    menus_leaving: HashMap<SharedString, u64>,
    /// Numbers those fades.
    menu_leave_gen: u64,
    /// Branches for the branch menu are being fetched (skeleton rows).
    branches_loading: bool,
    /// The hero's branch listing is current for this showing of the hero;
    /// reset when a chat is open so reopening the hero fetches again.
    branches_fresh: bool,
    /// Latest branch fetch; an older one's answer is dropped.
    branch_fetch_gen: u64,
    /// Last known branches per folder: a folder change shows its branch at
    /// once (the chip rolls on that frame) while a refresh runs behind it.
    branch_cache: HashMap<String, keel_rpc::BranchInfo>,
    /// Folders whose branches are being prefetched (folder menu rows).
    branch_prefetching: HashSet<String>,
    /// Why creating the new chat failed (dirty tree, ...), shown at the composer.
    create_error: Option<String>,
    /// A first message whose chat could not be created, put back in the composer.
    restore_draft: Option<String>,
    /// What was picked from the `@` menu (issues, files, folders): chips
    /// above the input, added to the message on send.
    pending_refs: Vec<MentionItem>,
    refs_leaving: HashMap<String, u64>,
    refs_gen: u64,
    chip_widths: std::rc::Rc<std::cell::RefCell<HashMap<String, f32>>>,
    /// Chips on their way out (key → exit number) stay in place while they
    /// fold away; widths are measured as they paint, for that fold. The row
    /// number replays the row's open when the first chip arrives.
    /// Folded rows toggled lately, for their open/close motion: row id →
    /// (toggle number, when). Old entries are dropped on the next toggle.
    fold_toggles: HashMap<String, (u64, std::time::Instant)>,
    fold_gen: u64,
    /// Folded rows (tool groups, thinking) the user opened, by row id.
    expanded_tools: HashSet<String>,
    /// ⌘⌥E: every turn shows its steps instead of "Show works".
    expand_steps: bool,
    /// Phase origin for trace shimmers, shared by every line.
    clock_epoch: std::time::Instant,
    /// Repaints trace motion while a fold glides or a step runs.
    motion_ticker: Option<gpui::Task<()>>,
    /// The transcript row under the mouse, for its message's Copy button.
    hovered_row: std::rc::Rc<RowHover>,
    /// A code block just copied, for the brief "Copied" check.
    copied: Option<(String, std::time::Instant)>,
    /// Images waiting in the composer, and how many are still being stored.
    pending_images: Vec<PendingImage>,
    attaching: usize,
    /// The last thumbnails while their row closes, the close's number, and
    /// the open's number (replayed when the first image arrives).
    images_leaving: Vec<PendingImage>,
    images_gen: u64,
    /// Thumbnails folding away from the middle of the row: where each sat,
    /// it, and its exit number.
    images_folding: Vec<(usize, PendingImage, u64)>,
    images_row_gen: u64,
    /// Full-size image or diagram overlay. Kept through its exit, which
    /// `lightbox_fx` plays.
    lightbox: Option<Lightbox>,
    lightbox_fx: crate::motion::Presence,
    /// Shells under the transcript.
    terminal: terminal_panel::TerminalPanel,
    browser: browser_panel::BrowserPanel,
    files: files_panel::FilesPanel,
    /// The always-open card over the chat's top right.
    session_card: session_card::SessionCard,
    /// The docked composer's slot width last frame.
    composer_slot: std::rc::Rc<std::cell::Cell<f32>>,
    /// The docked panel's width cap, as last laid out: where the hero drop
    /// aims, so it lands at the width the panel then keeps.
    composer_max: std::rc::Rc<std::cell::Cell<f32>>,
    /// The error toast, top center. Bumping `notice_gen` replays the entrance.
    status_message: Option<String>,
    notice_gen: u64,
    notice_leaving: bool,
    notice_hold: Option<(u64, Task<()>)>,
    notice_exit: Option<Task<()>>,
    /// The session timer (persisted) and its pill's view state.
    timer: crate::timer::SessionTimer,
    timer_ui: timer_pill::TimerUi,
    /// Per-session merge request monitor (persisted), its chip's view
    /// state, and the "Track merge request…" box.
    mr: crate::mr::MrStore,
    mr_ui: mr_chip::MrUi,
    mr_input: Entity<InputState>,
    /// The menu an outside click just closed. A click on that menu's own
    /// trigger arrives right after, and must not reopen it.
    menu_dismissed: Option<(&'static str, std::time::Instant)>,
    /// Chats with alerts you haven't looked at (sidebar marks, Dock badge).
    unread: crate::notify::Unread,
    /// The Dock badge as last set, so it is only touched on change.
    dock_label: Option<String>,
    sound_gate: crate::notify::SoundGate,
    /// The Keel window is the key window (from activation changes).
    window_active: bool,
    /// Settings → Notifications: the sound list is open; the system sounds.
    show_sound_menu: bool,
    system_sounds: Vec<String>,
    /// Settings → Background / sound import: the last failure.
    import_error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// Text fields paint text, placeholder, selection and caret only; the
/// container around each field owns its fill and outline.
fn editor_style(t: Tokens) -> InputEditorStyle {
    InputEditorStyle {
        foreground: c(t.text),
        muted_foreground: c(t.text_faint),
        background: gpui::transparent_black(),
        border: gpui::transparent_black(),
        selection: c(t.selection),
        caret: c(t.accent),
        ..Default::default()
    }
}

fn new_command_id() -> uuid::Uuid {
    uuid::Uuid::new_v4()
}

type GitFileStats = HashMap<String, (char, Option<u32>, Option<u32>)>;
static GIT_DIFF_CACHE: LazyLock<Mutex<HashMap<String, Arc<GitFileStats>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static GIT_DIFF_IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn fetch_git_numstats(root: &str) -> GitFileStats {
    let mut numstats: GitFileStats = HashMap::new();
    let root_path = std::path::Path::new(root);
    if root.is_empty() || !root_path.exists() {
        return numstats;
    }

    if let Ok(output) = std::process::Command::new("git")
        .args(["-C", root, "diff", "HEAD", "--numstat", "-z"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut records = text.split('\0');
            while let Some(record) = records.next() {
                let mut fields = record.splitn(3, '\t');
                let (Some(added), Some(removed), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
                    continue;
                };
                let path = if path.is_empty() {
                    records.next();
                    records.next().unwrap_or_default()
                } else {
                    path
                };
                numstats.insert(path.to_string(), ('M', added.parse().ok(), removed.parse().ok()));
            }
        }
    }

    if let Ok(output) = std::process::Command::new("git")
        .args(["-C", root, "status", "--porcelain", "-z"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            let mut entries = text.split('\0');
            while let Some(entry) = entries.next() {
                if entry.len() < 4 {
                    continue;
                }
                let xy = &entry[..2];
                let path = entry[3..].to_string();
                let status = if xy.contains('?') {
                    '?'
                } else if xy.contains('D') {
                    'D'
                } else if xy.contains('A') {
                    'A'
                } else {
                    'M'
                };
                if let Some(s) = numstats.get_mut(&path) {
                    s.0 = status;
                } else {
                    let lines = if status == '?' || status == 'A' {
                        std::fs::read_to_string(root_path.join(&path)).ok().map(|c| c.lines().count() as u32)
                    } else {
                        None
                    };
                    numstats.insert(path, (status, lines, Some(0)));
                }
            }
        }
    }

    numstats
}

impl KeelWindow {
    pub fn new(
        state: AppState,
        client: Option<RpcClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Read once: the OS query is an Objective-C round trip.
        let os_reduced = os_reduced_motion();
        let reduced = state.effective_reduced_motion(os_reduced);
        crate::style::set_zoom(state.ui_settings.zoom.unwrap_or(1.0));

        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder("Do anything…")
        });
        let question_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Type your answer… (Enter to submit)")
        });
        let rename_input = cx.new(|cx| InputState::new(window, cx).placeholder("Space name"));
        let gitlab_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter projects"));
        let picker_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let gitlab_host = cx.new(|cx| InputState::new(window, cx).placeholder("https://gitlab.com"));
        let browser_sites = cx.new(|cx| InputState::new(window, cx).placeholder("Empty allows every site · e.g. github.com, localhost:3000"));
        let gitlab_token = cx.new(|cx| InputState::new(window, cx).placeholder("Personal access token").masked(true));
        let mr_input = cx.new(|cx| InputState::new(window, cx).placeholder("Merge request link or !42"));
        let dotify_token = cx.new(|cx| InputState::new(window, cx).placeholder("Dotify token (with or without Bearer)").masked(true));
        let dotify_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects"));
        let dotify_desc = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 3).submit_on_enter(true).placeholder("Describe the work"));

        let browser = browser_panel::BrowserPanel::new(window, cx);
        let files = files_panel::FilesPanel::new(window, cx);
        let subscriptions = vec![
            cx.subscribe_in(&browser.address, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.browser_navigate(window, cx);
                }
            }),
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { shift: false, .. } => {
                        this.handle_primary_button(window, cx)
                    }
                    InputEvent::Change => this.on_composer_changed(cx),
                    _ => {}
                }
            }),
            cx.subscribe_in(&question_input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    this.submit_question_text(window, cx);
                }
            }),
            cx.subscribe_in(&rename_input, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.save_rename(cx);
                }
            }),
            cx.subscribe_in(&mr_input, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.mr_submit_prompt(cx);
                }
            }),
            // The project list filters as you type.
            cx.subscribe_in(&gitlab_filter, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&picker_filter, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.debounce_picker_query(cx);
                    // The model menu filters as you type.
                    if this.show_model_picker {
                        cx.notify();
                    }
                }
            }),
            cx.subscribe_in(&dotify_search, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.dotify_search_changed(cx);
                }
            }),
            cx.subscribe_in(&dotify_desc, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    if this.dotify.flow.view == crate::dotify::View::Edit { this.dotify_save_edit(cx) } else { this.dotify_clock_in(cx) }
                }
            }),
            cx.subscribe_in(&dotify_token, window, |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => {
                        if !this.dotify.connect_btn.busy() {
                            this.dotify_connect(window, cx);
                        }
                    }
                    InputEvent::Change => {
                        if this.dotify.connect_error.take().is_some() {
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            }),
            // Coming back to Keel reads the chat on screen (and syncs Dotify).
            cx.observe_window_activation(window, |this, window, cx| {
                this.window_active = window.is_window_active();
                if this.window_active {
                    this.mark_current_read(cx);
                    // The tree may have changed while away.
                    this.card_poke(cx);
                }
                this.dotify_on_activation(cx);
            }),
        ];
        // Global keystroke interceptors ahead of all focus, sheets, and inputs:
        // ⌘, toggles Settings (both opening and closing).
        // Model menu navigates search/selection while open.
        let weak = cx.entity().downgrade();
        let mut subscriptions = subscriptions;
        subscriptions.push(cx.intercept_keystrokes(move |event, window, cx| {
            let Some(this) = weak.upgrade() else { return };
            let ks = &event.keystroke;
            if ks.key == "," && ks.modifiers.secondary() && !ks.modifiers.shift && !ks.modifiers.alt && !ks.modifiers.control {
                this.update(cx, |this, cx| this.toggle_settings(cx));
                cx.stop_propagation();
                return;
            }
            if this.update(cx, |this, cx| this.model_menu_key(&event.keystroke, window, cx)) {
                cx.stop_propagation();
            }
        }));

        // Clicked banners and `keel://` links: Keel to the front, on that chat.
        // Also the banner permission: look it up (asking, when banners are on
        // and macOS hasn't asked yet), and redraw when it changes.
        crate::platform_notify::init_native(state.ui_settings.notifications.banner);
        let mut requests = crate::platform_notify::ui_requests();
        cx.spawn_in(window, async move |this, cx| {
            while let Some(request) = requests.recv().await {
                let done = this.update_in(cx, |this, window, cx| match request {
                    crate::platform_notify::UiRequest::OpenChat(chat_id) => this.open_from_banner(&chat_id, window, cx),
                    crate::platform_notify::UiRequest::Refresh => cx.notify(),
                });
                if done.is_err() {
                    break;
                }
            }
        })
        .detach();
        // Debug builds: commands from `ui-dev.sock`, run like the clicks they stand for.
        #[cfg(debug_assertions)]
        {
            let mut commands = crate::dev_control::commands();
            cx.spawn_in(window, async move |this, cx| {
                while let Some(command) = commands.recv().await {
                    let answer = this.update_in(cx, |this, window, cx| this.dev_command(&command.line, window, cx));
                    let Ok(answer) = answer else { break };
                    let _ = command.reply.send(answer);
                }
            })
            .detach();
        }
        // `KEEL_TEST_NOTIFY=<chat id>` (or `latest`): post one banner shortly
        // after launch, to try the banner and its click.
        if let Ok(target) = std::env::var("KEEL_TEST_NOTIFY") {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                let _ = this.update(cx, |this, _| {
                    let id = match target.as_str() {
                        "latest" | "" => this.state.step_chat(crate::state::AppState::now_ms(), 1),
                        id => Some(id.to_string()),
                    };
                    let title = id.as_ref().and_then(|id| this.state.chats.get(id)).map(|c| c.title.clone()).filter(|t| !t.is_empty());
                    let posted = crate::platform_notify::show_banner(
                        title.as_deref().unwrap_or("Keel"),
                        "Test notification: click to open this chat",
                        id.as_deref(),
                    );
                    let permission = crate::platform_notify::banner_permission();
                    tracing::info!(target: "keel_ui::notify", chat = ?id, ?permission, posted, "test banner");
                });
            })
            .detach();
        }
        if std::env::var_os("KEEL_DEMO_MODEL_LOADER").is_some() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                let _ = this.update(cx, |this, cx| {
                    // In memory only: the saved default harness is untouched.
                    if let Ok(harness) = std::env::var("KEEL_DEMO_MODEL_LOADER").map(|v| v.trim().to_string())
                        && harness != "1"
                        && this.current_chat.is_none()
                    {
                        this.selected_harness = harness.clone();
                        this.ensure_models(harness, cx);
                    }
                    this.show_model_picker = true;
                    this.kick_motion(cx);
                    cx.notify();
                });
            })
            .detach();
        }
        // `KEEL_TEST_CARD=permission|question|multi`: show a sample card in the
        // open chat shortly after launch (UI only: answering it reaches no run).
        let demo = std::env::var("KEEL_TEST_CARD")
            .ok()
            .or_else(|| std::env::var_os("KEEL_DEMO_PERMISSION").map(|_| "permission".to_string()));
        if let Some(kind) = demo {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                // On the new-chat screen, open the newest chat first.
                let _ = this.update(cx, |this, cx| {
                    if this.current_chat.is_none() {
                        match this.state.ui_settings.open_tabs.first().cloned() {
                            Some(tab) => this.state.active_tab = Some(tab),
                            None => {
                                if let Some(id) = this.state.step_chat(crate::state::AppState::now_ms(), 1) {
                                    this.state.open_or_focus_tab(&id);
                                }
                            }
                        }
                        cx.notify();
                    }
                });
                cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                let _ = this.update(cx, |this, cx| {
                    let Some(chat) = this.current_chat.clone().or_else(|| this.state.active_tab.clone()) else {
                        tracing::info!(target: "keel_ui::approval", tabs = this.state.ui_settings.open_tabs.len(), "test card: no open chat");
                        return;
                    };
                    let q = crate::approval::sample_question(&kind);
                    this.question_panels.insert(chat.clone(), QuestionPanelState::new("test-card".into(), vec![q]));
                    this.focus_question = true;
                    tracing::info!(target: "keel_ui::approval", %chat, %kind, synced = this.current_chat.is_some(), "test card shown");
                    cx.notify();
                });
                // Then fold it away (UI only: nothing is sent), to run the exit;
                // the receipt then stays put for a look.
                cx.background_executor().timer(std::time::Duration::from_secs(8)).await;
                let _ = this.update(cx, |this, cx| {
                    let Some(chat) = this.current_chat.clone().or_else(|| this.state.active_tab.clone()) else { return };
                    let Some(panel) = this.question_panels.remove(&chat) else { return };
                    let Some(q) = panel.current_question().cloned() else { return };
                    let pick = q.options.iter().position(|o| o.role == keel_proto::OptionRole::AllowOnce).unwrap_or(0);
                    let answer = keel_proto::UserInputAnswer { question_id: q.id.clone(), kind: q.kind, value: q.options.get(pick).map(|o| o.id.clone()).unwrap_or_default() };
                    let receipt = keel_proto::Receipt::for_answer(&q, &answer);
                    tracing::info!(target: "keel_ui::approval", receipt = %crate::approval::receipt_line(&receipt).text(), height = this.card_height.get(), "test card answered");
                    this.demo_hold_exit = true;
                    this.begin_question_exit(chat, panel, Some(0), receipt, false, cx);
                });
            })
            .detach();
        }
        // `KEEL_SIDEBAR_BENCH=n` (with `KEEL_FRAME_LOG`): toggle the sidebar n
        // times (even keeps it as it was), 1.5 s apart, to time the motion.
        if let Some(n) = std::env::var("KEEL_SIDEBAR_BENCH").ok().and_then(|v| v.parse::<u32>().ok()) {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(3)).await;
                // `KEEL_SIDEBAR_BENCH_CHAT`: time it over a chat, not the hero.
                if std::env::var_os("KEEL_SIDEBAR_BENCH_CHAT").is_some() {
                    let _ = this.update(cx, |this, cx| {
                        if let Some(id) = this.state.step_chat(crate::state::AppState::now_ms(), 1) {
                            this.close_pages();
                            this.finish_settings_exit(cx);
                            this.state.open_or_focus_tab(&id);
                            cx.notify();
                        }
                    });
                }
                cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                for _ in 0..n {
                    if this.update(cx, |this, cx| this.toggle_sidebar(cx)).is_err() {
                        break;
                    }
                    cx.background_executor().timer(std::time::Duration::from_millis(1500)).await;
                }
            })
            .detach();
        }
        let timer = crate::timer::SessionTimer::load(&state.data_dir);
        let mr = crate::mr::MrStore::load(&state.data_dir);
        let projects = projects::Projects::load(&state.data_dir);
        let mut this = Self {
            state,
            client,
            transcript: TranscriptState::new(reduced),
            current_chat: None,
            chat_gen: 0,
            chat_reveal: None,
            chat_stream: None,
            pending_events: HashMap::new(),
            messages: HashMap::new(),
            msg_order: Vec::new(),
            dirty: BTreeSet::new(),
            parse_in_flight: false,
            turn_chips: HashMap::new(),
            doc_views: HashMap::new(),
            turn_gen: 0,
            publish_queue: Vec::new(),
            publish_task: None,
            step_break: false,
            pending_first_message: None,
            composer,
            draft_save: None,
            prompt_suggestion: None,
            suggestion_revision: 0,
            suggestion_key: None,
            suggestion_task: None,
            composer_placeholder: "Do anything…".to_string(),
            chat_summaries: HashMap::new(),
            summary_hover: Default::default(),
            question_panels: HashMap::new(),
            question_input,
            focus_question: false,
            question_exit: None,
            card_height: Default::default(),
            question_stopped: Default::default(),
            mr_merges: HashMap::new(),
            armed_runs: Default::default(),
            subagents: BTreeMap::new(),
            subagent_open: None,
            subagent_caps: keel_proto::SubagentCaps::default(),
            show_subagent_menu: false,
            show_points_menu: false,
            demo_hold_exit: false,
            rename_input,
            focus_handle: cx.focus_handle(),
            input_style_for: None,
            harnesses: Vec::new(),
            models: HashMap::new(),
            models_loading: HashSet::new(),
            models_ready_at: HashMap::new(),
            // `KEEL_DEMO_MODEL_LOADER=1` holds the skeleton; `=<harness id>`
            // (e.g. `claude_code`) opens that harness's real rows instead.
            demo_model_loader: std::env::var("KEEL_DEMO_MODEL_LOADER").is_ok_and(|v| v == "1"),
            models_error: HashMap::new(),
            selected_harness: String::new(),
            selected_model: String::new(),
            selected_effort: String::new(),
            selected_permission: Default::default(),
            show_permission_menu: false,
            permission_prev: Default::default(),
            permission_roll: 0,
            model_chip_w: Default::default(),
            permission_chip_w: Default::default(),
            show_harness_picker: false,
            show_model_picker: false,
            show_effort_picker: false,
            show_effort_menu: false,
            effort_from: 0.,
            effort_glide: 0,
            effort_prev: String::new(),
            model_prev: String::new(),
            model_roll: 0,
            model_roll_up: true,
            branch_chip_w: None,
            composer_h: None,
            send_fill: None,
            composer_natural: Default::default(),
            composer_h_hero: false,
            composer_line_h: 0.,
            composer_fit: Default::default(),
            composer_fit_known: false,
            composer_fit_at: None,
            composer_panel_w: Default::default(),
            composer_ctrl_w: Default::default(),
            branch_label: crate::motion::RollingLabel::default(),
            branch_label_w: 0.,
            branch_prev_w: 0.,
            os_reduced_motion: os_reduced,
            resizing_sidebar: false,
            row_order: HashMap::new(),
            row_order_filter: None,
            row_glide: HashMap::new(),
            row_enter: HashMap::new(),
            space_fold_gen: HashMap::new(),
            glide_gen: 0,
            latest_pill: crate::motion::Presence::hidden(),
            awaiting_first_output: false,
            loader_verbs: main_pane::THINKING_VERBS,
            loader_seed: (std::time::Instant::now(), 0),
            loader_row: None,
            loader_at: None,
            row_motion_once: false,
            loader_exit: None,
            tail_len: 0,
            sidebar_anim: None,
            frame_now: Default::default(),
            column_slide: None,
            column_last: Default::default(),
            geo_trace: Default::default(),
            projects,
            sidebar_from: 0.,
            right_panel_anim: None,
            right_panel_from: 0.,
            frame_probe: Default::default(),
            was_hero: false,
            hero_exit: None,
            hero_path: Default::default(),
            hero_gen: 0,
            hero_panel: Default::default(),
            model_chip_bounds: Default::default(),
            effort_chip_bounds: Default::default(),
            permission_chip_bounds: Default::default(),
            card_branch_bounds: Default::default(),
            dock_h: Default::default(),
            tail_dock_h: 0.,
            hero_title: Default::default(),
            dock_slot: Default::default(),
            hero_from: None,
            creating_chat: false,
            sidebar_peek: false,
            peek_closing: false,
            peek_close: None,
            peek_gen: 0,
            gitlab_open: false,
            gitlab: Default::default(),
            gitlab_filter,
            settings_open: false,
            last_settings_toggle: None,
            settings_leaving: None,
            settings_gen: 0,
            page_key: 0,
            page_from: 0,
            page_at: None,
            harness_status: None,
            sign_in: None,
            auth_landed: None,
            connections: Vec::new(),
            gitlab_host,
            browser_sites,
            browser_sites_synced: false,
            gitlab_token,
            dotify: Default::default(),
            dotify_token,
            dotify_search,
            dotify_desc,
            connect_busy: false,
            connect_error: None,
            switch_flips: HashMap::new(),
            switch_gen: 0,
            mention: None,
            mention_shown: None,
            mention_gen: 0,
            mention_leaving: None,
            mention_leave_gen: 0,
            folder_paths: HashMap::new(),
            commands: HashMap::new(),
            commands_loading: HashSet::new(),
            folder_paths_loading: HashSet::new(),
            zoom_toast: None,
            zoom_toast_task: None,
            select_mode: false,
            selected_chats: Vec::new(),
            select_anchor: None,
            confirming_delete_chats: None,
            delete_confirm_gen: 0,
            delete_confirm_leaving: None,
            select_mode_gen: 0,
            select_leaving: None,
            rows_exiting: Vec::new(),
            rows_exit_gen: 0,
            fullscreen: false,
            issue_repo: None,
            repo_projects: None,
            repo_projects_loading: false,
            repo_issues: None,
            picker_filter,
            picker_query: String::new(),
            picker_filter_task: None,
            model_menu: Default::default(),
            menus_open: HashSet::new(),
            menus_leaving: HashMap::new(),
            menu_leave_gen: 0,
            branches_loading: false,
            branches_fresh: false,
            branch_fetch_gen: 0,
            branch_cache: HashMap::new(),
            branch_prefetching: HashSet::new(),
            create_error: None,
            restore_draft: None,
            pending_refs: Vec::new(),
            refs_leaving: HashMap::new(),
            refs_gen: 0,
            chip_widths: Default::default(),
            fold_toggles: HashMap::new(),
            fold_gen: 0,
            expanded_tools: HashSet::new(),
            expand_steps: false,
            clock_epoch: std::time::Instant::now(),
            motion_ticker: None,
            copied: None,
            hovered_row: Default::default(),
            pending_images: Vec::new(),
            attaching: 0,
            images_leaving: Vec::new(),
            images_gen: 0,
            images_folding: Vec::new(),
            images_row_gen: 0,
            lightbox: None,
            lightbox_fx: crate::motion::Presence::hidden(),
            terminal: terminal_panel::TerminalPanel::new(cx.focus_handle()),
            browser,
            files,
            session_card: Default::default(),
            composer_slot: Default::default(),
            composer_max: Default::default(),
            status_message: None,
            notice_gen: 0,
            notice_leaving: false,
            notice_hold: None,
            notice_exit: None,
            timer,
            timer_ui: Default::default(),
            mr,
            mr_ui: Default::default(),
            mr_input,
            menu_dismissed: None,
            unread: Default::default(),
            dock_label: None,
            sound_gate: Default::default(),
            window_active: true,
            show_sound_menu: false,
            system_sounds: Vec::new(),
            import_error: None,
            _subscriptions: subscriptions,
        };
        this.start_registry_subscription(cx);
        this.load_harnesses(cx);
        // A timer left running keeps counting (wall clock) and ticking.
        this.ensure_timer_tick(cx);
        this.mr_start(cx);
        this.dotify_boot(cx);
        this.dotify_dev_hooks(cx);
        // Typing works the moment the window opens.
        this.composer.update(cx, |s, cx| s.focus(window, cx));
        this
    }

    /// Store images on the host and queue them in the composer.
    pub(super) fn attach_paths(&mut self, mut paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let room = MAX_IMAGES.saturating_sub(self.pending_images.len() + self.attaching);
        if paths.len() > room {
            paths.truncate(room);
            self.report("can't attach", format!("up to {MAX_IMAGES} images per message"));
        }
        if !paths.is_empty() && self.pending_images.is_empty() && self.attaching == 0 {
            self.images_row_gen += 1;
            self.images_leaving.clear();
        }
        for path in paths {
            self.attaching += 1;
            let client = client.clone();
            cx.spawn(async move |this, cx| {
                let res = client
                    .call("attach_file", serde_json::json!({ "path": path.to_string_lossy() }))
                    .await
                    .and_then(|v| serde_json::from_value::<keel_proto::Attachment>(v).map_err(|e| e.to_string()));
                let _ = this.update(cx, |this, cx| {
                    this.attaching = this.attaching.saturating_sub(1);
                    match res {
                        Ok(attachment) => {
                            if !this.pending_images.iter().any(|p| p.attachment.id == attachment.id) {
                                this.pending_images.push(PendingImage { attachment, path });
                            }
                        }
                        Err(err) => this.report("can't attach", err),
                    }
                    // A failed lone attach takes its spinner with it.
                    this.close_images_row(Vec::new(), cx);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// A thumbnail's ×: the last one closes the row with it; any other
    /// folds its width away in place, its neighbours sliding over.
    pub(super) fn remove_image(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(ix) = self.pending_images.iter().position(|p| p.attachment.id == id) else {
            return;
        };
        let image = self.pending_images.remove(ix);
        if self.pending_images.is_empty() && self.attaching == 0 {
            self.close_images_row(vec![image], cx);
        } else if !self.reduced() {
            self.images_gen += 1;
            let n = self.images_gen;
            self.images_folding.push((ix, image, n));
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(crate::motion::SLOW + Duration::from_millis(20)).await;
                let _ = this.update(cx, |this, cx| {
                    this.images_folding.retain(|f| f.2 != n);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// When the last image left (`gone`, the thumbnails to fold away with
    /// the row), the row closes, then lets them go.
    fn close_images_row(&mut self, gone: Vec<PendingImage>, cx: &mut Context<Self>) {
        if !self.pending_images.is_empty() || self.attaching > 0 || gone.is_empty() {
            return;
        }
        self.images_leaving = gone;
        self.images_folding.clear();
        self.images_gen += 1;
        let n = self.images_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::motion::SLOW + Duration::from_millis(20)).await;
            let _ = this.update(cx, |this, cx| {
                if this.images_gen == n {
                    this.images_leaving.clear();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// ⌘V with an image (or copied image files) on the clipboard. True when handled.
    pub(super) fn paste_images(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else {
            return false;
        };
        let mut paths = Vec::new();
        for entry in item.entries() {
            match entry {
                gpui::ClipboardEntry::Image(image) => {
                    let ext = match image.format {
                        gpui::ImageFormat::Jpeg => "jpg",
                        gpui::ImageFormat::Gif => "gif",
                        gpui::ImageFormat::Webp => "webp",
                        _ => "png",
                    };
                    let dir = self.state.data_dir.join("tmp");
                    let file = dir.join(format!("paste-{}.{ext}", uuid::Uuid::new_v4()));
                    if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&file, &image.bytes).is_ok() {
                        paths.push(file);
                    }
                }
                gpui::ClipboardEntry::ExternalPaths(external) => {
                    paths.extend(external.paths().iter().filter(|p| is_image_path(p)).cloned());
                }
                _ => {}
            }
        }
        if paths.is_empty() {
            return false;
        }
        self.attach_paths(paths, cx);
        true
    }

    pub(super) fn pick_images(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |this, cx| this.attach_paths(paths, cx));
            }
        })
        .detach();
    }

    /// Reduced motion as currently in effect (OS value or the menu override).
    /// A docked sidebar open/close is in motion (never under reduced motion).
    pub(super) fn sidebar_toggling(&self) -> bool {
        !self.reduced() && self.sidebar_anim.is_some_and(|(at, _)| self.slide_elapsed(at) < self.sidebar_duration())
    }

    /// The sheet beat for the current toggle: the open travels a little
    /// longer than the close; a turnaround mid-slide, over less distance,
    /// takes less of it.
    pub(super) fn sidebar_duration(&self) -> std::time::Duration {
        let full = if self.state.ui_settings.sidebar_collapsed { crate::motion::SHEET_CLOSE } else { crate::motion::SHEET_OPEN };
        full.mul_f32(0.4 + 0.6 * (self.sidebar_target() - self.sidebar_from).abs())
    }

    pub(super) fn sidebar_target(&self) -> f32 {
        if self.state.ui_settings.sidebar_collapsed { 0. } else { 1. }
    }

    /// Time since a slide started, on this frame's clock (`frame_now`).
    pub(super) fn slide_elapsed(&self, at: std::time::Instant) -> std::time::Duration {
        self.frame_now.get().map_or_else(|| at.elapsed(), |now| now.saturating_duration_since(at))
    }

    /// How open the sliding sidebar is right now.
    fn sidebar_open_now(&self) -> f32 {
        if !self.sidebar_toggling() {
            return self.sidebar_target();
        }
        self.sidebar_from + (self.sidebar_target() - self.sidebar_from) * self.sidebar_progress()
    }

    /// The current sidebar slide's eased progress (0 start, 1 landed).
    fn sidebar_progress(&self) -> f32 {
        let Some((at, _)) = self.sidebar_anim.filter(|_| !self.reduced()) else { return 1. };
        let t = (self.slide_elapsed(at).as_secs_f32() / self.sidebar_duration().as_secs_f32()).min(1.);
        crate::motion::glide_curve(t)
    }

    /// The current right panel slide's eased progress (0 start, 1 landed).
    fn right_panel_progress(&self) -> f32 {
        let Some((at, _)) = self.right_panel_anim.filter(|_| !self.reduced()) else { return 1. };
        let t = (self.slide_elapsed(at).as_secs_f32() / self.right_panel_duration().as_secs_f32()).min(1.);
        crate::motion::glide_curve(t)
    }

    /// How far the reading column is through its slide: the slower of the
    /// two when the sidebar and a right panel move at once.
    pub(super) fn slide_progress(&self) -> f32 {
        let mut p: f32 = 1.;
        if self.sidebar_toggling() {
            p = p.min(self.sidebar_progress());
        }
        if self.right_panel_toggling() {
            p = p.min(self.right_panel_progress());
        }
        p
    }

    /// Whether any right side panel (browser, files, terminal) is currently open and allowed.
    pub(super) fn right_panel_open(&self) -> bool {
        self.browser_allowed() && (self.browser.open || self.files.open || self.terminal.open)
    }

    /// Target open fraction for the right side panel (0.0 or 1.0).
    pub(super) fn right_panel_target(&self) -> f32 {
        if self.right_panel_open() { 1. } else { 0. }
    }

    /// The duration for the right panel animation, scaling with distance if reversed mid-flight.
    /// Opening uses [`crate::motion::PANEL_OPEN`] (260 ms) and closing uses
    /// [`crate::motion::PANEL_CLOSE`] (200 ms) for snappy dismissals.
    pub(super) fn right_panel_duration(&self) -> std::time::Duration {
        let base = if self.right_panel_target() >= self.right_panel_from {
            crate::motion::PANEL_OPEN
        } else {
            crate::motion::PANEL_CLOSE
        };
        base.mul_f32(0.4 + 0.6 * (self.right_panel_target() - self.right_panel_from).abs())
    }

    /// Whether the right side panel is currently animating open or shut.
    pub(super) fn right_panel_toggling(&self) -> bool {
        !self.reduced() && self.right_panel_anim.is_some_and(|(at, _)| self.slide_elapsed(at) < self.right_panel_duration())
    }

    /// How open the right side panel is right now (0.0 to 1.0).
    pub(super) fn right_panel_open_now(&self) -> f32 {
        if self.reduced() {
            return self.right_panel_target();
        }
        let Some((at, _)) = self.right_panel_anim else { return self.right_panel_target() };
        let dur = self.right_panel_duration();
        let elapsed = at.elapsed();
        if elapsed >= dur {
            return self.right_panel_target();
        }
        let t = (elapsed.as_secs_f32() / dur.as_secs_f32()).min(1.);
        self.right_panel_from + (self.right_panel_target() - self.right_panel_from) * crate::motion::glide_curve(t)
    }

    /// Called right before toggling any right side panel to capture the starting position.
    pub(super) fn update_right_panel_anim(&mut self) {
        let open_now = if self.right_panel_toggling() {
            self.right_panel_open_now()
        } else if self.right_panel_open() {
            1.
        } else {
            0.
        };
        self.right_panel_from = open_now;
        let next = self.right_panel_anim.map_or(1, |(_, n)| n + 1);
        self.right_panel_anim = Some((std::time::Instant::now(), next));
    }

    /// Trigger right panel entrance or exit animation on session/hero transition.
    pub(super) fn sync_right_panel_hero_transition(&mut self, hero: bool) {
        let any_open = self.browser.open || self.files.open || self.terminal.open;
        if !any_open || self.reduced() {
            return;
        }
        let from = if hero {
            // Exiting chat to hero: panel glides from current open fraction to 0.0.
            if self.right_panel_toggling() {
                self.right_panel_open_now()
            } else {
                1.0
            }
        } else {
            // Entering chat from hero: panel glides from 0.0 to 1.0.
            if self.right_panel_toggling() {
                self.right_panel_open_now()
            } else {
                0.0
            }
        };
        self.right_panel_from = from;
        let now = std::time::Instant::now();
        let next = self.right_panel_anim.map_or(1, |(_, n)| n + 1);
        self.right_panel_anim = Some((now, next));
        if self.browser.open {
            self.browser.anim = Some((now, next));
        }
        if self.files.open {
            self.files.anim = Some((now, next));
        }
        if self.terminal.open {
            self.terminal.anim = Some((now, next));
        }
    }

    /// The reading column's max width. Constant: a slide holds the column's
    /// actual width through `column_slide` instead.
    pub(super) fn column_max(&self) -> gpui::Pixels {
        crate::style::TRANSCRIPT_MAX_WIDTH
    }

    fn reduced(&self) -> bool {
        self.transcript.reduced_motion
    }

    /// Left inset that clears the traffic lights, or a plain margin in fullscreen.
    fn traffic_lights_inset(&self) -> gpui::Pixels {
        if self.fullscreen { gpui::px(8.) } else { crate::style::TRAFFIC_LIGHTS_WIDTH }
    }

    /// Zoom the whole UI. Every fixed size reads the factor on the next
    /// render; the transcript's cached row heights are thrown away.
    pub(super) fn set_zoom(&mut self, factor: f32, cx: &mut Context<Self>) {
        crate::style::set_zoom(factor);
        let factor = crate::style::zoom();
        self.state.ui_settings.zoom = ((factor - 1.0).abs() > 0.001).then_some(factor);
        self.state.save_settings();
        self.transcript.height_memo.clear();
        self.transcript.list_state.remeasure();
        self.input_style_for = None;
        // Show the level; each change restarts the pill's fade and timer.
        let generation = self.zoom_toast.map_or(1, |(n, _)| n + 1);
        self.zoom_toast = Some((generation, self.zoom_toast.is_some()));
        self.zoom_toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ZOOM_TOAST + Duration::from_millis(50)).await;
            let _ = this.update(cx, |this, cx| {
                if this.zoom_toast.is_some_and(|(n, _)| n == generation) {
                    this.zoom_toast = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// Open or close the sidebar; the render animates the width.
    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_peek = false;
        self.peek_closing = false;
        self.peek_close = None;
        let open_now = if self.sidebar_toggling() {
            self.sidebar_open_now()
        } else if self.state.ui_settings.sidebar_collapsed {
            0.
        } else {
            1.
        };
        self.sidebar_from = open_now;
        self.state.toggle_sidebar();
        let next = self.sidebar_anim.map_or(1, |(_, n)| n + 1);
        self.sidebar_anim = Some((std::time::Instant::now(), next));
        // One render after the sheet drops the animating shell (and, on
        // collapse, the zero-width clip).
        let settle = self.sidebar_duration();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(settle + std::time::Duration::from_millis(20))
                .await;
            let _ = this.update(cx, |_, cx| {
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Close-by-outside-click bookkeeping for toggle triggers.
    /// Where the menu `key` is this frame: `None` hidden, `Some(None)` open,
    /// `Some(Some(n))` playing exit `n`. Read against last frame, so a close
    /// from any path (a click away, Escape, a pick) plays the exit.
    fn menu_phase(&mut self, key: impl Into<SharedString>, open: bool, cx: &mut Context<Self>) -> Option<Option<u64>> {
        let key = key.into();
        if open {
            self.menus_leaving.remove(&key);
            self.menus_open.insert(key);
            return Some(None);
        }
        if self.menus_open.remove(&key) && !self.reduced() {
            self.menu_leave_gen += 1;
            let n = self.menu_leave_gen;
            self.menus_leaving.insert(key.clone(), n);
            let k = key.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(crate::motion::FAST + Duration::from_millis(20)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.menus_leaving.get(&k) == Some(&n) {
                        this.menus_leaving.remove(&k);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.menus_leaving.get(&key).map(|n| Some(*n))
    }

    fn note_menu_dismissed(&mut self, menu: &'static str) {
        self.menu_dismissed = Some((menu, std::time::Instant::now()));
    }

    /// True when `menu` was closed by the press that is now clicking its trigger.
    fn menu_just_dismissed(&mut self, menu: &'static str) -> bool {
        match self.menu_dismissed.take() {
            Some((id, at)) => id == menu && at.elapsed() < Duration::from_millis(400),
            None => false,
        }
    }

    fn report<E: std::fmt::Display>(&mut self, what: &str, err: E) {
        tracing::warn!("{what}: {err}");
        let message = format!("{what}: {err}");
        // The same error again (a retry loop) keeps the toast still.
        if self.notice_leaving || self.status_message.as_ref() != Some(&message) {
            self.notice_gen += 1;
            self.notice_leaving = false;
            self.notice_exit = None;
        }
        self.status_message = Some(message);
    }

    /// Play the toast's exit, then drop it.
    pub(super) fn dismiss_notice(&mut self, cx: &mut Context<Self>) {
        if self.status_message.is_none() || self.notice_leaving {
            return;
        }
        self.notice_leaving = true;
        let generation = self.notice_gen;
        self.notice_exit = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NOTICE_EXIT + Duration::from_millis(20)).await;
            let _ = this.update(cx, |this, cx| {
                if this.notice_gen == generation && this.notice_leaving {
                    this.status_message = None;
                    this.notice_leaving = false;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    // ── Active chat & subscription ──────────────────────────────────────

    /// Make the transcript, composer draft, pickers and subscription follow
    /// `state.active_tab`. Called at the top of every render, so every path
    /// that changes the active tab (sidebar, tab strip, close, new session,
    /// startup restore, space deletion) goes through here.
    fn sync_active_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.restore_draft.take()
            && self.current_chat.is_none()
            && self.composer.read(cx).value().trim().is_empty()
        {
            self.composer.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        if let Some(active) = self.state.active_tab.clone()
            && !self.state.ui_settings.open_tabs.contains(&active)
        {
            self.state.active_tab = self.state.ui_settings.open_tabs.first().cloned();
        }
        let want = self.state.active_tab.clone();
        if want == self.current_chat {
            return;
        }

        // Persist the outgoing chat's draft right away.
        if let Some(old) = self.current_chat.take() {
            let text = self.composer.read(cx).value().to_string();
            self.state.set_draft(&old, &text);
        }
        self.draft_save = None;

        self.current_chat = want.clone();
        self.mark_current_read(cx);
        self.chat_gen += 1;
        // Rows land over a few frames and the list only finds the end after
        // measuring them: hidden until then, so a switch never shows the top
        // of the chat and then jumps. A fresh chat has nothing to load.
        self.chat_reveal = want
            .as_ref()
            .and_then(|id| self.state.chats.get(id))
            .is_some_and(|chat| chat.last_message_at != 0)
            .then_some(None);
        self.chat_stream = None;
        self.pending_events.clear();
        self.messages.clear();
        self.msg_order.clear();
        self.dirty.clear();
        self.turn_chips.clear();
        self.doc_views.clear();
        self.publish_queue.clear();
        self.publish_task = None;
        self.step_break = false;
        self.transcript.clear();
        self.awaiting_first_output = false;
        self.invalidate_suggestion();
        self.mr_on_switch(cx);
        self.subagents.clear();
        self.subagent_open = None;
        self.show_subagent_menu = false;
        self.subagent_caps = keel_proto::SubagentCaps::default();

        let draft = want.as_deref().map(|id| self.state.draft(id)).unwrap_or_default();
        self.composer.update(cx, |s, cx| {
            s.set_value(draft, window, cx);
            s.focus(window, cx);
        });

        let Some(chat_id) = want else {
            self.apply_harness_prefs();
            return;
        };
        if let Some(chat) = self.state.chats.get(&chat_id).cloned() {
            self.adopt_chat_options(&chat, cx);
        }
        self.focus_question = self.question_panels.contains_key(&chat_id);
        self.subscribe_chat(chat_id.clone(), cx);
        self.fetch_subagent_caps(chat_id.clone(), cx);
        self.ensure_git_stats(cx);

        if let Some((pending_chat, text)) = self.pending_first_message.take() {
            if pending_chat == chat_id {
                self.send_text(&chat_id, text, false, cx);
            } else {
                self.pending_first_message = Some((pending_chat, text));
            }
        }
    }

    /// Pickers follow the active chat when its options are known.
    fn adopt_chat_options(&mut self, chat: &ChatItem, cx: &mut Context<Self>) {
        if !chat.harness.is_empty() {
            self.selected_harness = chat.harness.clone();
            self.ensure_models(chat.harness.clone(), cx);
        }
        if !chat.model.is_empty() {
            self.selected_model = chat.model.clone();
        }
        // An older row has no reasoning field. Leave the chip alone rather
        // than treating that absence as the harness default.
        if chat.reasoning_known {
            self.selected_effort = chat.reasoning.clone();
            // A session saved on the retired poteto effort: map it now if the
            // models are in (else `ensure_models` does when they arrive).
            if let Some(list) = self.models.get(&self.selected_harness) {
                self.selected_effort = list.coerce_effort(&self.selected_model, &self.selected_effort);
            }
        }
        self.selected_permission = keel_proto::PermissionMode::parse(&chat.permission);
    }

    /// Modes the selected harness honours (just Ask before the list loads).
    fn permission_modes(&self) -> &[keel_proto::PermissionMode] {
        self.harnesses
            .iter()
            .find(|h| h.id == self.selected_harness)
            .map(|h| h.permission_modes.as_slice())
            .filter(|modes| !modes.is_empty())
            .unwrap_or(&[keel_proto::PermissionMode::Ask])
    }

    /// The picked mode if this harness has it, else Ask.
    fn effective_permission(&self) -> keel_proto::PermissionMode {
        Some(self.selected_permission).filter(|m| self.permission_modes().contains(m)).unwrap_or_default()
    }

    /// A new chat starts from what was last picked for its harness. Models
    /// not loaded yet are checked when they arrive (`ensure_models`).
    fn apply_harness_prefs(&mut self) {
        let Some(prefs) = self.state.ui_settings.harness_prefs.get(&self.selected_harness).cloned() else {
            return;
        };
        if !prefs.model.is_empty() {
            self.selected_model = prefs.model;
        }
        self.selected_effort = prefs.effort;
        if let Some(list) = self.models.get(&self.selected_harness) {
            if list.get(&self.selected_model).is_none() {
                self.selected_model = list.initial_model();
            }
            self.selected_effort = list.coerce_effort(&self.selected_model, &self.selected_effort);
        }
        self.selected_permission = keel_proto::PermissionMode::parse(&prefs.permission);
    }

    /// Saves the pickers as this harness's last choice.
    fn remember_harness_prefs(&mut self) {
        if self.selected_harness.is_empty() {
            return;
        }
        let old = self.state.ui_settings.harness_prefs.get(&self.selected_harness);
        let model_name = match self.models.get(&self.selected_harness).and_then(|l| l.get(&self.selected_model)) {
            Some(m) => m.name.clone(),
            None => old.filter(|p| p.model == self.selected_model).map(|p| p.model_name.clone()).unwrap_or_default(),
        };
        let prefs = keel_proto::HarnessPrefs {
            model: self.selected_model.clone(),
            model_name,
            effort: self.selected_effort.clone(),
            permission: self.selected_permission.as_str().to_string(),
        };
        let slot = self.state.ui_settings.harness_prefs.entry(self.selected_harness.clone()).or_default();
        if *slot != prefs {
            *slot = prefs;
            self.state.save_settings();
        }
    }

    fn select_permission(&mut self, mode: keel_proto::PermissionMode, cx: &mut Context<Self>) {
        let before = self.effective_permission();
        self.selected_permission = mode;
        if self.effective_permission() != before {
            self.permission_prev = before;
            self.permission_roll += 1;
        }
        self.remember_harness_prefs();
        self.show_permission_menu = false;
        self.push_chat_options(cx);
        cx.notify();
    }

    fn subscribe_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let generation = self.chat_gen;
        cx.spawn(async move |this, cx| {
            let res = client.subscribe_chat(&chat_id).await;
            let _ = this.update(cx, |this, cx| {
                if this.chat_gen != generation {
                    return; // switched away meanwhile; that stream stays ignored
                }
                match res {
                    Ok(stream) => {
                        let buffered = this.pending_events.remove(&stream).unwrap_or_default();
                        this.pending_events.clear();
                        this.chat_stream = Some(stream);
                        for event in buffered {
                            this.apply_chat_event(event, cx);
                        }
                    }
                    Err(err) => this.report("subscribe_chat failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What the active chat's harness allows for subagents. The engine
    /// answers for the harness it hosts; a chat on another device reports
    /// nothing (panel hidden).
    fn fetch_subagent_caps(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let generation = self.chat_gen;
        cx.spawn(async move |this, cx| {
            let caps = client.subagent_caps(&chat_id).await.ok();
            let _ = this.update(cx, |this, cx| {
                if this.chat_gen == generation
                    && let Some(caps) = caps
                {
                    this.subagent_caps = caps;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The active chat's subagent list, oldest first.
    pub(super) fn subagent_list(&self) -> Vec<&serde_json::Value> {
        self.subagents.values().collect()
    }

    /// The open subagent's record, if it is still around.
    pub(super) fn open_subagent(&self) -> Option<&serde_json::Value> {
        let id = self.subagent_open.as_deref()?;
        self.subagents.get(id)
    }

    pub(super) fn open_subagent_session(&mut self, subagent_id: &str, cx: &mut Context<Self>) {
        self.subagent_open = Some(subagent_id.to_string());
        self.show_subagent_menu = false;
        cx.notify();
    }

    pub(super) fn close_subagent_session(&mut self, cx: &mut Context<Self>) {
        self.subagent_open = None;
        cx.notify();
    }

    pub(super) fn toggle_subagent_menu(&mut self, cx: &mut Context<Self>) {
        self.show_subagent_menu = !self.show_subagent_menu;
        cx.notify();
    }

    pub fn start_registry_subscription(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<RpcStreamItem>(1024);
        client.set_stream_sender(tx);
        cx.spawn(async move |this, cx| {
            if let Err(err) = client.subscribe_registry().await {
                tracing::warn!("subscribe_registry failed: {err}");
            }
            if let Err(err) = client.subscribe_browser().await {
                tracing::warn!("subscribe_browser failed: {err}");
            }
            while let Some(item) = rx.recv().await {
                if this.update(cx, |this, cx| this.on_stream_item(item, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Route one stream item: registry paths update the sidebar; session
    /// events go to the transcript only when they belong to the active chat's
    /// stream (or are buffered while that stream id is still unknown).
    fn on_stream_item(&mut self, item: RpcStreamItem, cx: &mut Context<Self>) {
        match &item.event {
            SubscribeEvent::MirrorDiff { path, view, .. } => {
                if let Some(id) = path.strip_prefix("spaces.") {
                    self.state.update_space_from_view(id, view);
                    cx.notify();
                    return;
                }
                if let Some(id) = path.strip_prefix("chats.") {
                    let before = self.state.chats.get(id).map(|chat| chat.status);
                    self.state.update_chat_from_view(id, view);
                    let after = self.state.chats.get(id).filter(|chat| !chat.tombstoned).map(|chat| chat.status);
                    match (before, after) {
                        (Some(before), Some(after)) => {
                            if let Some(event) = crate::notify::NotifyEvent::from_status_change(before, after) {
                                self.on_chat_alert(id.to_string(), event, cx);
                            }
                            if before == ChatStatus::Running && after != ChatStatus::Running {
                                self.files_on_turn_end(id, cx);
                                self.card_on_turn_end(id, cx);
                                self.invalidate_git_stats();
                                self.refresh_git_stats(cx);
                                self.record_chat_summary_on_turn_end(id);
                                // Its last, held-back block lands now.
                                if self.current_chat.as_deref() == Some(id)
                                    && let Some(last) = self.msg_order.last().cloned()
                                {
                                    self.publish_rows(&last);
                                }
                            }
                        }
                        (_, None) => {
                            if self.unread.mark_read(id) {
                                self.refresh_dock_badge();
                            }
                        }
                        _ => {}
                    }
                    cx.notify();
                    return;
                }
                if let Some(id) = path.strip_prefix("diffs.") {
                    self.state.update_diff_from_view(id, view);
                    self.files_on_checkout_changed(id, cx);
                    self.card_on_checkout(id, cx);
                    cx.notify();
                    return;
                }
            }
            SubscribeEvent::StreamClosed { .. } => return,
            SubscribeEvent::TerminalOutput { .. } | SubscribeEvent::TerminalExit { .. } => {
                self.on_terminal_event(item, cx);
                return;
            }
            SubscribeEvent::BrowserRequest { .. } => {
                self.on_browser_host_request(item.event, cx);
                return;
            }
            SubscribeEvent::BrowserCancel { request_id, chat } => {
                self.on_browser_cancel(request_id.clone(), chat.clone(), cx);
                return;
            }
            SubscribeEvent::Live { .. } => {}
        }
        if self.chat_stream.as_deref() == Some(item.stream.as_str()) {
            self.apply_chat_event(item.event, cx);
        } else if self.chat_stream.is_none() && self.current_chat.is_some() {
            self.pending_events.entry(item.stream).or_default().push(item.event);
        }
    }

    /// A chat finished, needs input, or failed: badge it, and bounce, banner,
    /// or chime per Settings → Notifications, unless you're looking at it.
    fn on_chat_alert(&mut self, chat_id: String, event: crate::notify::NotifyEvent, cx: &mut Context<Self>) {
        use crate::notify::{NotifyEvent, should_alert, should_play_sound};
        let settings = self.state.ui_settings.notifications.clone();
        let on_screen = self.current_chat.as_deref() == Some(chat_id.as_str()) && !self.settings_open && !self.gitlab_open;
        if !should_alert(event, &settings, on_screen, self.window_active) {
            return;
        }
        self.unread.mark(&chat_id, event);
        self.refresh_dock_badge();
        if event == NotifyEvent::NeedsInput && settings.bounce {
            crate::platform_notify::bounce_dock();
        }
        if settings.banner {
            let title = self.state.chats.get(&chat_id).map(|chat| chat.title.clone()).filter(|t| !t.is_empty());
            crate::platform_notify::show_banner(title.as_deref().unwrap_or("Chat"), event.message(), Some(&chat_id));
        }
        if should_play_sound(&settings, crate::platform_notify::app_active())
            && self.sound_gate.allow(std::time::Instant::now())
            && let Some(path) = crate::background::sound_path(&self.state.data_dir, &settings.sound)
        {
            crate::platform_notify::play_sound(&path, settings.volume);
        }
        cx.notify();
    }

    /// A banner was clicked (or a `keel://` link opened): bring Keel to the
    /// front and open its chat, if that chat is still around.
    fn open_from_banner(&mut self, chat_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        cx.activate(true);
        window.activate_window();
        if let crate::notify::BannerAction::OpenChat(id) =
            crate::notify::banner_action(chat_id, |id| self.state.chats.contains_key(id))
        {
            self.close_pages();
            self.finish_settings_exit(cx);
            self.state.open_or_focus_tab(&id);
        }
        cx.notify();
    }

    /// The open chat is read (when the window is in front to show it).
    pub(super) fn mark_current_read(&mut self, cx: &mut Context<Self>) {
        if !self.window_active || self.settings_open || self.gitlab_open {
            return;
        }
        if let Some(id) = self.current_chat.clone()
            && self.unread.mark_read(&id)
        {
            self.refresh_dock_badge();
            cx.notify();
        }
    }

    /// The Dock badge from the unread chats and the badge style.
    pub(super) fn refresh_dock_badge(&mut self) {
        let label = self.unread.dock_label(self.state.ui_settings.notifications.badge);
        if label != self.dock_label {
            crate::platform_notify::set_dock_badge(label.as_deref());
            self.dock_label = label;
        }
    }

    /// One message's doc view (rebuilt from push frames) onto its transcript entry.
    fn apply_message_view(&mut self, id: &str, view: &serde_json::Value, cx: &mut Context<Self>) {
        let Some(text) = crate::turn::view_text(view) else {
            return;
        };
        let text = text.as_str();
        let role = view.get("role").and_then(|v| v.as_str()).unwrap_or("assistant");
        let images = ImageRef::parse_list(view);
        let entry = self.message_entry(id, role);
        entry.blocks.set_text(text);
        // The local echo already shows them; the doc confirms or fills in.
        if !images.is_empty() && entry.images != images {
            entry.images = images;
        }
        if entry.role == "assistant" {
            // Steps in stream order; live text ahead of the doc is kept.
            let old = std::mem::take(&mut entry.steps);
            entry.steps = crate::turn::merge_doc(id, old, crate::turn::doc_steps(view));
        }
        // Tools and thinking need no parse: show them now.
        self.publish_rows(id);
        self.mark_dirty(id.to_string(), cx);
        self.invalidate_suggestion();
        self.ensure_git_stats(cx);
    }

    fn apply_chat_event(&mut self, event: SubscribeEvent, cx: &mut Context<Self>) {
        match event {
            SubscribeEvent::MirrorDiff { path, view, .. } if path.starts_with("subagents.") => {
                if let Some(id) = path.strip_prefix("subagents.") {
                    self.subagents.insert(id.to_string(), view.clone());
                }
            }
            SubscribeEvent::MirrorDiff { path, view: frame, .. } if path.starts_with("messages.") => {
                let Some(id) = frame.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
                    return;
                };
                let Some(view) = crate::turn::apply_message_frame(self.doc_views.remove(&id), &frame) else {
                    tracing::warn!(message = %id, "message push frame out of step; waiting for a whole one");
                    return;
                };
                self.apply_message_view(&id, &view, cx);
                self.doc_views.insert(id, view);
                self.kick_motion(cx);
            }
            SubscribeEvent::Live { agent_event } => match agent_event {
                AgentEvent::Text { message_id, delta, .. } => {
                    self.invalidate_suggestion();
                    self.awaiting_first_output = false;
                    self.loader_verbs = main_pane::WRITING_VERBS;
                    let new_step = std::mem::take(&mut self.step_break);
                    let entry = self.message_entry(&message_id, "assistant");
                    entry.blocks.full_text.push_str(&delta);
                    crate::turn::push_text(&mut entry.steps, &message_id, &delta, new_step);
                    self.mark_dirty(message_id, cx);
                }
                // The engine pushes the message with its tool parts right after
                // each tool event; the group is drawn from that.
                AgentEvent::ToolStarted { name, kind, subagent, .. } => {
                    self.step_break = true;
                    self.awaiting_first_output = false;
                    // A subagent's own tools leave the loader on "Waiting on a subagent".
                    if subagent.is_none() {
                        self.loader_verbs = main_pane::tool_verbs(&name, kind.as_deref().unwrap_or_default());
                    }
                }
                AgentEvent::ToolFinished { .. } => {}
                // Progress and plans also arrive as the engine's message push.
                AgentEvent::ToolProgress { .. } | AgentEvent::Plan { .. } => {}
                // The engine also pushes the subagent's record (with its
                // transcript) right after each of these; the panel and the
                // indicator row are drawn from that.
                AgentEvent::SubagentStarted { .. } | AgentEvent::SubagentDone { .. } => {
                    let event_is_start = matches!(agent_event, AgentEvent::SubagentStarted { .. });
                    self.step_break = true;
                    self.awaiting_first_output = false;
                    self.loader_verbs = match event_is_start {
                        true => main_pane::tool_verbs("", "subagent"),
                        false => main_pane::WORKING_VERBS,
                    };
                }
                // The registry row carries the totals; its diff redraws the meter.
                AgentEvent::Usage { .. } => {}
                AgentEvent::Question { id, questions, subagent: None } => {
                    self.awaiting_first_output = false;
                    if let Some(chat) = self.current_chat.clone() {
                        self.question_stopped.remove(&chat);
                        if self.question_exit.as_ref().is_some_and(|e| e.chat == chat) {
                            self.question_exit = None;
                        }
                        self.question_panels.insert(chat, QuestionPanelState::new(id, questions));
                        self.focus_question = true;
                    }
                }
                // A subagent's own question is shown in the subagent panel.
                AgentEvent::Question { .. } => {}
                AgentEvent::Done { status } => {
                    self.invalidate_suggestion();
                    self.awaiting_first_output = false;
                    // The turn is over: its file chips and MR links get a fresh look.
                    self.turn_gen += 1;
                    self.invalidate_git_stats();
                    self.refresh_git_stats(cx);
                    if let Some(chat) = self.current_chat.as_ref() {
                        if let Some(item) = self.state.chats.get_mut(chat) {
                            item.status = match status {
                                TurnStatus::Completed {} | TurnStatus::Interrupted {} => ChatStatus::Idle,
                                TurnStatus::Aborted {} | TurnStatus::Errored { .. } => ChatStatus::Aborted,
                            };
                        }
                    }
                    if let Some(last) = self
                        .msg_order
                        .iter()
                        .rev()
                        .find(|m| self.messages.get(*m).is_some_and(|e| e.role == "assistant"))
                        .cloned()
                    {
                        self.row_motion_once = true;
                        self.publish_rows(&last);
                        self.row_motion_once = false;
                    }
                    if let Some(chat) = self.current_chat.clone() {
                        // A card still waiting when the turn ends was canceled.
                        if let Some(panel) = self.question_panels.remove(&chat) {
                            let receipt = panel
                                .current_question()
                                .map(|q| keel_proto::Receipt::ended(q, keel_proto::Decision::Canceled));
                            if let Some(receipt) = receipt {
                                self.begin_question_exit(chat.clone(), panel, None, receipt, true, cx);
                            }
                        }
                        self.question_stopped.remove(&chat);
                    }
                    // A failed turn says why, inline, instead of just stopping.
                    let reason = match status {
                        TurnStatus::Errored { message } => Some(message),
                        TurnStatus::Aborted {} => Some("turn aborted".to_string()),
                        TurnStatus::Completed {} | TurnStatus::Interrupted {} => None,
                    };
                    if let Some(reason) = reason {
                        let id = format!("error-{}", uuid::Uuid::new_v4());
                        let entry = self.message_entry(&id, "error");
                        entry.blocks.set_text(&format!("⚠ {reason}"));
                        self.mark_dirty(id, cx);
                    }
                    if let Some(chat) = self.current_chat.clone() {
                        self.record_chat_summary_on_turn_end(&chat);
                    }
                }
                AgentEvent::Reasoning { message_id, delta, .. } => {
                    self.loader_verbs = main_pane::THINKING_VERBS;
                    let new_step = std::mem::take(&mut self.step_break);
                    crate::turn::push_reasoning(&mut self.message_entry(&message_id, "assistant").steps, &delta, new_step);
                    // Batched: a delta per token would rebuild the turn's rows per token.
                    self.queue_publish(message_id, cx);
                }
            },
            _ => {}
        }
        cx.notify();
    }

    fn message_entry(&mut self, id: &str, role: &str) -> &mut MsgEntry {
        if !self.messages.contains_key(id) {
            self.msg_order.push(id.to_string());
        }
        self.messages.entry(id.to_string()).or_insert_with(|| MsgEntry {
            role: role.to_string(),
            blocks: MessageBlockState::new(id.to_string()),
            steps: Vec::new(),
            images: Vec::new(),
        })
    }

    /// Publish the rows `id` belongs to. A user or error message is its own
    /// block. An assistant turn (consecutive assistant messages, which a
    /// driver may split around tool calls) is one block: its thinking, tool
    /// groups and text interleaved in stream order, then file/result chips.
    fn publish_rows(&mut self, id: &str) {
        let Some(entry) = self.messages.get(id) else {
            return;
        };
        // Rows that land while the open chat runs are live: they glide in,
        // and rows they replace fade out. Loading or idle, rows just appear.
        self.transcript.animate_new = self.active_status() == Some(ChatStatus::Running) || std::mem::take(&mut self.row_motion_once);
        if entry.role != "assistant" {
            let mut rows = Vec::new();
            if !entry.images.is_empty() {
                rows.push(TranscriptBlock::Images { id: format!("{id}#images"), items: entry.images.clone() });
            }
            rows.extend(entry.blocks.all_blocks());
            let role = entry.role.clone();
            let text = (role == "user").then(|| entry.blocks.full_text.clone());
            let later = self.later_messages(id);
            self.transcript.upsert_message_before(id, &role, rows, &later);
            // A link the user pastes is offered too.
            if let Some(text) = text {
                self.mr_scan(id, &text, false);
            }
            return;
        }
        let Some(pos) = self.msg_order.iter().position(|m| m == id) else {
            return;
        };
        let assistant = |m: &String| self.messages.get(m).is_some_and(|e| e.role == "assistant");
        let (start, end) = turn_span(&self.msg_order, pos, assistant);
        let turn = &self.msg_order[start..=end];
        // The turn's rows live under its first message's id.
        let head = turn[0].clone();
        let mut rows: Vec<TranscriptBlock> = Vec::new();
        let expanded = |row_id: &str| self.expanded_tools.contains(row_id);
        let mut finished = 0usize;
        // The live turn is still streaming.
        let streaming = end + 1 == self.msg_order.len() && self.active_status() == Some(ChatStatus::Running);
        for m in turn {
            let Some(entry) = self.messages.get(m) else {
                continue;
            };
            finished += entry
                .steps
                .iter()
                .filter_map(|step| match step {
                    crate::turn::Step::Tools(tools) => Some(tools.iter().filter(|t| t.state != "running").count()),
                    _ => None,
                })
                .sum::<usize>();
            // Thinking, tools and text interleaved, in the order they streamed.
            crate::turn::push_rows(m, &entry.steps, &expanded, &mut rows);
        }
        let key = TurnChipKey { finished, turn_gen: self.turn_gen };
        let chips = match self.turn_chips.get(&head) {
            Some((k, chips)) if *k == key => chips.clone(),
            _ => {
                let chips = self.scan_turn_chips(turn.to_vec(), &head);
                self.turn_chips.insert(head.clone(), (key, chips.clone()));
                chips
            }
        };
        // Extract any FileChanges card so it is pinned at the end of the final response once finished.
        let mut file_changes = Vec::new();
        rows.retain(|row| {
            if matches!(row, TranscriptBlock::FileChanges { .. }) {
                file_changes.push(row.clone());
                false
            } else {
                true
            }
        });
        for row in &mut file_changes {
            if let TranscriptBlock::FileChanges { id, expanded, .. } = row {
                *expanded = self.expanded_tools.contains(id.as_str());
            }
        }
        let mut chips = chips;
        if let Some(TranscriptBlock::FileChanges { id, expanded, .. }) = &mut chips {
            *expanded = self.expanded_tools.contains(id.as_str());
        }
        // A finished turn's steps fold into one expandable "Show works" / "Hide works" line.
        if !streaming {
            let summary_id = format!("{head}#worked");
            let open = self.expand_steps || self.expanded_tools.contains(&summary_id);
            if let Some(summary) = worked_summary(&rows, summary_id, open) {
                rows = fold_turn_rows(rows, summary, open);
            }
            if !file_changes.is_empty() {
                rows.extend(file_changes);
            } else {
                rows.extend(chips);
            }
        } else if file_changes.is_empty() {
            rows.extend(chips);
        }
        let later = self.later_messages(&head);
        self.transcript.upsert_message_before(&head, "assistant", rows, &later);
    }

    /// The messages after `id` in chat order.
    fn later_messages(&self, id: &str) -> Vec<String> {
        let pos = self.msg_order.iter().position(|m| m == id).map_or(self.msg_order.len(), |pos| pos + 1);
        self.msg_order[pos..].to_vec()
    }

    /// Trigger git stats refresh if not already cached for the workspace root.
    pub(super) fn ensure_git_stats(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.mention_root().filter(|r| !r.is_empty()) else { return };
        if GIT_DIFF_CACHE.lock().unwrap().contains_key(&root) {
            return;
        }
        self.refresh_git_stats(cx);
    }

    /// Invalidate cached git stats for the current chat's root so fresh stats are fetched.
    pub(super) fn invalidate_git_stats(&mut self) {
        if let Some(root) = self.mention_root() {
            GIT_DIFF_CACHE.lock().unwrap().remove(&root);
        }
    }

    /// Fetch git numstats and status asynchronously off the UI thread and update turn chips on completion.
    pub(super) fn refresh_git_stats(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.mention_root().filter(|r| !r.is_empty()) else { return };
        {
            let mut in_flight = GIT_DIFF_IN_FLIGHT.lock().unwrap();
            if !in_flight.insert(root.clone()) {
                return;
            }
        }
        let generation = self.chat_gen;
        cx.spawn(async move |this, cx| {
            let root_for_bg = root.clone();
            let stats = cx.background_spawn(async move {
                fetch_git_numstats(&root_for_bg)
            }).await;
            let _ = this.update(cx, |this, cx| {
                GIT_DIFF_CACHE.lock().unwrap().insert(root.clone(), Arc::new(stats));
                GIT_DIFF_IN_FLIGHT.lock().unwrap().remove(&root);
                if this.chat_gen != generation {
                    return;
                }
                this.turn_gen += 1;
                for id in this.msg_order.clone() {
                    if this.messages.get(&id).is_some_and(|e| e.role == "assistant") {
                        this.publish_rows(&id);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The turn's file chips, and its MR links offered (or tracked when one
    /// of its tools made one). Scans every message of the turn and stats the
    /// candidate files, so `publish_rows` caches the result per turn.
    fn scan_turn_chips(&mut self, turn: Vec<String>, head: &str) -> Option<TranscriptBlock> {
        let root = self.mention_root().unwrap_or_default();
        let root_path = std::path::Path::new(&root);

        // Files the turn wrote: named by finished write tools or by an answer
        // line that says it created one. Kept only if they exist.
        let mut written: Vec<String> = Vec::new();
        // MR links in the turn, and whether one of its tools made one.
        let (mut mr_text, mut mr_created) = (String::new(), false);
        // In-memory diff line counts from tools in this turn (0ms, pure memory)
        let mut in_memory_stats: std::collections::HashMap<String, (char, Option<u32>, Option<u32>)> = std::collections::HashMap::new();

        for m in &turn {
            let Some(entry) = self.messages.get(m) else {
                continue;
            };
            written.extend(crate::files::candidates_in_text(&entry.blocks.full_text));
            mr_text.push_str(&entry.blocks.full_text);
            mr_text.push('\n');
            for step in &entry.steps {
                let crate::turn::Step::Tools(tools) = step else { continue };
                for tool in tools {
                    written.extend(crate::files::candidates_in_tool(&tool.name, &tool.state, &tool.summary));
                    // A screenshot the tool returned gets a chip too.
                    written.extend(tool.images.iter().cloned());
                    mr_created |= crate::mr::tool_creates_mr(&tool.name, &tool.summary);
                    mr_text.push_str(&tool.summary);
                    mr_text.push('\n');

                    if let Some(diff) = &tool.diff {
                        let (mut add, mut rem) = (0u32, 0u32);
                        let old_str = diff.old_text.as_deref().unwrap_or("");
                        for (variant, _) in crate::code_block::diff_lines(old_str, &diff.new_text) {
                            match variant {
                                crate::code_block::LineVariant::Added => add += 1,
                                crate::code_block::LineVariant::Removed => rem += 1,
                                _ => {}
                            }
                        }
                        let status = if diff.old_text.is_none() { 'A' } else { 'M' };
                        let rel = std::path::Path::new(&diff.path)
                            .strip_prefix(root_path)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|_| diff.path.clone());
                        let val = {
                            let stats_entry = in_memory_stats.entry(rel.clone()).or_insert(('M', Some(0), Some(0)));
                            if let Some(cur_add) = stats_entry.1.as_mut() {
                                *cur_add += add;
                            }
                            if let Some(cur_rem) = stats_entry.2.as_mut() {
                                *cur_rem += rem;
                            }
                            if status == 'A' {
                                stats_entry.0 = 'A';
                            }
                            *stats_entry
                        };
                        if rel != diff.path {
                            in_memory_stats.insert(diff.path.clone(), val);
                        }
                    }
                }
            }
        }
        let roots = self.file_roots();
        let files = if written.is_empty() { Vec::new() } else { crate::files::resolve_all(written, &roots) };
        self.mr_scan(head, &mr_text, mr_created);
        if files.is_empty() {
            return None;
        }

        // Check static cache without blocking UI thread
        let cached_stats = if !root.is_empty() {
            GIT_DIFF_CACHE.lock().unwrap().get(&root).cloned()
        } else {
            None
        };

        let changes: Vec<crate::file_changes::FileChange> = files
            .into_iter()
            .map(|f| {
                let rel = std::path::Path::new(&f.path)
                    .strip_prefix(root_path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or(f.path.clone());
                let (status, added, removed) = if let Some(git) = &cached_stats {
                    git.get(&rel).copied().unwrap_or_else(|| {
                        in_memory_stats.get(&rel).or_else(|| in_memory_stats.get(&f.path)).copied().unwrap_or(('M', None, None))
                    })
                } else if let Some(&(s, a, r)) = in_memory_stats.get(&rel).or_else(|| in_memory_stats.get(&f.path)) {
                    (s, a, r)
                } else {
                    ('M', None, None)
                };
                crate::file_changes::FileChange {
                    path: rel,
                    status,
                    added,
                    removed,
                }
            })
            .collect();

        let id = format!("{head}#files");
        let expanded = self.expanded_tools.contains(&id);
        Some(TranscriptBlock::FileChanges {
            id,
            card: crate::file_changes::FileChanges {
                root,
                files: changes,
                more: 0,
            },
            expanded,
        })
    }

    /// Publish `id`'s rows on the next batch (about two frames out), with
    /// whatever else queued meanwhile.
    fn queue_publish(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.publish_queue.contains(&id) {
            self.publish_queue.push(id);
        }
        if self.publish_task.is_some() {
            return;
        }
        self.publish_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PUBLISH_BATCH).await;
            let _ = this.update(cx, |this, cx| {
                this.publish_task = None;
                for id in std::mem::take(&mut this.publish_queue) {
                    this.publish_rows(&id);
                }
                cx.notify();
            });
        }));
    }

    /// Where a relative path an agent mentions can live: the chat's folder
    /// and, for a worktree chat, `{data_dir}/worktrees/*/{chat_id}`.
    fn file_roots(&self) -> Vec<std::path::PathBuf> {
        let mut roots: Vec<std::path::PathBuf> = self.mention_root().into_iter().map(Into::into).collect();
        if let Some(chat) = &self.current_chat
            && let Ok(repos) = std::fs::read_dir(self.state.data_dir.join("worktrees"))
        {
            roots.extend(repos.flatten().map(|r| r.path().join(chat)).filter(|p| p.is_dir()));
        }
        roots
    }

    /// A file chip was clicked: open it in the right-side files panel editor.
    pub(super) fn open_file_result(&mut self, path: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let is_image = matches!(
            crate::files::FileKind::of(&path),
            crate::files::FileKind::Image | crate::files::FileKind::Svg
        );
        if is_image && path.is_file() {
            let dark = matches!(window.appearance(), gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark);
            let t = self.state.effective_tokens(dark);
            let preview = (cfg!(target_os = "macos") && self.browser_allowed())
                .then(|| crate::files::preview_html(&path, t.window, t.text))
                .flatten();
            if let Some(html) = preview {
                self.browser_preview(path, html, window, cx);
                return;
            }
        }
        self.show_file(path.to_string_lossy().to_string(), window, cx);
    }

    /// Open or close a folded row (`{message_id}#tools` / `#reasoning`).
    pub(super) fn toggle_row(&mut self, row_id: &str, cx: &mut Context<Self>) {
        // A trace line sends the state the user chose: `{line}=1` or `=0`.
        let base = match row_id.rsplit_once('=') {
            Some((base, "0" | "1")) => {
                self.expanded_tools.remove(&format!("{base}=0"));
                self.expanded_tools.remove(&format!("{base}=1"));
                self.expanded_tools.insert(row_id.to_string());
                base
            }
            _ => {
                if !self.expanded_tools.remove(row_id) {
                    self.expanded_tools.insert(row_id.to_string());
                }
                row_id
            }
        };
        self.fold_gen += 1;
        self.fold_toggles.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(1));
        self.fold_toggles.insert(base.to_string(), (self.fold_gen, std::time::Instant::now()));
        // Line ids are `{message}#{n}~{tool}`: the message is before the row's `#`.
        let row = base.split_once('~').map_or(base, |(row, _)| row);
        if let Some((message_id, _)) = row.rsplit_once('#') {
            let message_id = message_id.to_string();
            // Opening or closing "Worked for …": its steps glide in or fade
            // out, even with the chat idle.
            self.row_motion_once = row.ends_with("#worked");
            self.publish_rows(&message_id);
            self.row_motion_once = false;
        }
        self.kick_motion(cx);
        cx.notify();
    }

    /// The newest task steps in the conversation, shown under the session card.
    pub(super) fn current_task_steps(&self) -> Option<crate::steps::Steps> {
        for msg_id in self.msg_order.iter().rev() {
            if let Some(entry) = self.messages.get(msg_id) {
                for step in entry.steps.iter().rev() {
                    if let crate::turn::Step::Steps { steps, .. } = step {
                        if steps.kind == crate::steps::StepKind::Task {
                            return Some(steps.clone());
                        }
                    }
                }
            }
        }
        None
    }

    /// Trace lines the user opened or closed, by line id.
    pub(super) fn open_lines(&self) -> HashMap<String, bool> {
        self.expanded_tools
            .iter()
            .filter_map(|id| match id.rsplit_once('=')? {
                (base, "1") => Some((base.to_string(), true)),
                (base, "0") => Some((base.to_string(), false)),
                _ => None,
            })
            .collect()
    }

    /// ⌘⌥E: show or hide the steps of every turn.
    pub(super) fn toggle_expand_steps(&mut self, cx: &mut Context<Self>) {
        self.expand_steps = !self.expand_steps;
        let heads: Vec<String> = self.msg_order.clone();
        for id in heads {
            if self.messages.get(&id).is_some_and(|e| e.role == "assistant") {
                self.row_motion_once = true;
                self.publish_rows(&id);
                self.row_motion_once = false;
            }
        }
        self.kick_motion(cx);
        cx.notify();
    }

    /// Repaint while trace motion is live: 60fps while a fold glides, 30fps
    /// while only shimmers run, nothing when idle or the app is in the
    /// background. One task; it ends itself when nothing moves.
    pub(super) fn kick_motion(&mut self, cx: &mut Context<Self>) {
        if self.motion_ticker.is_some() || self.reduced() {
            return;
        }
        self.motion_ticker = Some(cx.spawn(async move |this, cx| loop {
            let Ok(frame) = this.update(cx, |this, cx| {
                let folding = this.fold_toggles.values().any(|(_, at)| at.elapsed() < Duration::from_millis(900));
                let running = this.transcript.items.iter().any(crate::trace_view::row_animating);
                // The model picker's skeleton, and its crossfade into rows.
                let picker = this.show_model_picker
                    && (this.demo_model_loader
                        || this.models_loading.contains(&this.selected_harness)
                        || this.models_ready_at.get(&this.selected_harness).is_some_and(|at| at.elapsed() < Duration::from_millis(MODEL_FADE_MS + 40)));
                let folding = folding || picker || this.model_menu_animating();
                if !folding && !running {
                    this.motion_ticker = None;
                    return None;
                }
                if cx.active_window().is_some() {
                    cx.notify();
                }
                Some(if folding { 16 } else { 33 })
            }) else {
                return;
            };
            let Some(ms) = frame else { return };
            cx.background_executor().timer(Duration::from_millis(ms)).await;
        }));
    }

    /// Copy a code block; its button shows a check for a moment.
    pub(super) fn copy_code(&mut self, row_id: String, content: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(content));
        self.mark_copied(row_id, cx);
    }

    /// A diagram as a PNG. Drawing and encoding take ~1s for a big chart, so
    /// they run in the background; "Copied" shows when it is on the clipboard.
    pub(super) fn copy_image(&mut self, row_id: String, source: String, tokens: Tokens, cx: &mut Context<Self>) {
        let Some(job) = crate::mermaid::png_job(&source, &tokens) else { return };
        cx.spawn(async move |this, cx| {
            let Some(png) = cx.background_spawn(async move { job() }).await else { return };
            let _ = this.update(cx, |this, cx| {
                let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, png);
                cx.write_to_clipboard(gpui::ClipboardItem::new_image(&image));
                this.mark_copied(row_id, cx);
            });
        })
        .detach();
    }

    fn mark_copied(&mut self, row_id: String, cx: &mut Context<Self>) {
        self.copied = Some((row_id, std::time::Instant::now()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copied.as_ref().is_some_and(|(_, at)| at.elapsed() >= Duration::from_millis(1400)) {
                    this.copied = None;
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ── Markdown parsing: background, coalesced ─────────────────────────

    fn mark_dirty(&mut self, message_id: String, cx: &mut Context<Self>) {
        self.dirty.insert(message_id);
        self.kick_parse(cx);
    }

    /// At most one parse job in flight. Deltas that arrive meanwhile only
    /// append text and mark the message dirty; the next job picks them up.
    fn kick_parse(&mut self, cx: &mut Context<Self>) {
        if self.parse_in_flight || self.dirty.is_empty() {
            return;
        }
        // One job per text: a user/error message's whole text (`None`), or
        // each text step of an assistant message (`Some(ordinal)`).
        let mut jobs: Vec<(String, Option<usize>, MessageBlockState)> = Vec::new();
        for id in std::mem::take(&mut self.dirty) {
            let Some(entry) = self.messages.get(&id) else {
                continue;
            };
            if entry.role == "assistant" {
                let texts = entry.steps.iter().filter_map(|s| match s {
                    crate::turn::Step::Text(state) => Some(state),
                    _ => None,
                });
                // Only texts with new text since their last parse (in practice
                // the open, streaming one): sealed steps are not cloned again.
                jobs.extend(
                    texts
                        .enumerate()
                        .filter(|(_, state)| state.needs_parse())
                        .map(|(ix, state)| (id.clone(), Some(ix), state.clone())),
                );
            } else {
                jobs.push((id, None, entry.blocks.clone()));
            }
        }
        if jobs.is_empty() {
            return;
        }
        self.parse_in_flight = true;
        let generation = self.chat_gen;
        cx.spawn(async move |this, cx| {
            let parsed = cx
                .background_spawn(async move {
                    jobs.into_iter()
                        .map(|(id, ix, mut s)| {
                            s.reparse_tail();
                            (id, ix, s)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.parse_in_flight = false;
                if this.chat_gen == generation {
                    let mut touched: Vec<String> = Vec::new();
                    for (id, ix, parsed) in parsed {
                        if this.apply_parsed(&id, ix, parsed) && !touched.contains(&id) {
                            touched.push(id);
                        }
                    }
                    for id in touched {
                        this.publish_rows(&id);
                    }
                }
                this.kick_parse(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Store a finished parse. False when the text it parsed was replaced
    /// meanwhile (that text is already dirty again).
    fn apply_parsed(&mut self, id: &str, ordinal: Option<usize>, mut parsed: MessageBlockState) -> bool {
        let Some(entry) = self.messages.get_mut(id) else {
            return false;
        };
        let target = match ordinal {
            None => Some(&mut entry.blocks),
            Some(ix) => entry
                .steps
                .iter_mut()
                .filter_map(|s| match s {
                    crate::turn::Step::Text(state) => Some(state),
                    _ => None,
                })
                .nth(ix),
        };
        let Some(target) = target else {
            return false;
        };
        if target.message_id != parsed.message_id || !target.full_text.starts_with(parsed.full_text.as_str()) {
            return false;
        }
        if target.full_text.len() > parsed.full_text.len() {
            // More text arrived while parsing: keep the progress, parse again.
            parsed.full_text = target.full_text.clone();
            self.dirty.insert(id.to_string());
        }
        *target = parsed;
        true
    }

    // ── Composer ────────────────────────────────────────────────────────

    fn on_composer_changed(&mut self, cx: &mut Context<Self>) {
        self.update_mention(cx);
        if let Some(chat) = self.current_chat.clone() {
            let text = self.composer.read(cx).value().to_string();
            self.draft_save = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(400)).await;
                let _ = this.update(cx, |this, _| this.state.set_draft(&chat, &text));
            }));
        }
        cx.notify();
    }

    /// Open, update or close the `@` menu from the text before the caret.
    fn update_mention(&mut self, cx: &mut Context<Self>) {
        let (text, cursor) = {
            let input = self.composer.read(cx);
            (input.value().to_string(), input.cursor())
        };
        let (at, query, slash) = match crate::mention::slash_query(&text, cursor) {
            Some((at, query)) => (at, query, true),
            None => match crate::mention::mention_query(&text, cursor) {
                Some((at, query)) => (at, query, false),
                None => {
                    self.mention = None;
                    return;
                }
            },
        };
        let query = query.to_string();
        let active = match &self.mention {
            Some(m) if m.at == at && m.query == query && m.slash == slash => m.active,
            _ => 0,
        };
        let opening = !self.mention.as_ref().is_some_and(|m| m.slash);
        self.mention = Some(Mention { at, query, active, slash });
        if slash {
            // Fresh each time the menu opens (a skill added since shows up);
            // the last list stays up meanwhile.
            self.ensure_commands(opening, cx);
            // Mid-sentence a `/word` is often just text: the menu only comes up
            // when something matches, so it never sits empty or eats Enter.
            let loaded = self.commands.contains_key(&self.commands_key());
            if at > 0 && loaded && self.mention_matches().is_empty() {
                self.mention = None;
            }
        } else if let Some(dirs) = self.projects.mention_dirs(self.current_chat.as_deref(), self.mention_root().as_deref()) {
            for dir in dirs {
                self.ensure_folder_paths(dir, cx);
            }
        } else if let Some(root) = self.mention_root() {
            self.ensure_folder_paths(root, cx);
        }
    }

    /// The folder `@` paths are relative to: the open chat's, or the folder
    /// the next chat will run in.
    pub(super) fn mention_root(&self) -> Option<String> {
        match self.current_chat.as_ref().and_then(|id| self.state.chats.get(id)) {
            Some(chat) => self.state.spaces.get(&chat.space_id).map(|s| s.path.clone()),
            None => self.state.target_space_for_new_session().map(|s| s.path),
        }
    }

    /// The harness a `/` command goes to: the open chat's, else the one the
    /// next chat will use.
    pub(super) fn slash_harness(&self) -> String {
        self.current_chat
            .as_ref()
            .and_then(|id| self.state.chats.get(id))
            .map(|c| c.harness.clone())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| self.selected_harness.clone())
    }

    /// Cache key for the `/` menu's list: harness and folder.
    pub(super) fn commands_key(&self) -> String {
        format!("{}\0{}", self.slash_harness(), self.mention_root().unwrap_or_default())
    }

    fn ensure_commands(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let key = self.commands_key();
        if (self.commands.contains_key(&key) && !refresh) || !self.commands_loading.insert(key.clone()) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let params = serde_json::json!({ "harness": self.slash_harness(), "cwd": self.mention_root() });
        cx.spawn(async move |this, cx| {
            let res = client.call("list_commands", params).await;
            let _ = this.update(cx, |this, cx| {
                this.commands_loading.remove(&key);
                let list = res
                    .map(|v| v["commands"].as_array().map(|r| r.iter().map(SlashCommand::parse).collect()).unwrap_or_default())
                    .unwrap_or_else(|err| {
                        this.report("list_commands failed", err);
                        Vec::new()
                    });
                this.commands.insert(key, list);
                // Re-judge the open `/` against the list that just landed (a
                // mid-sentence one with no match closes).
                if this.mention.as_ref().is_some_and(|m| m.slash) {
                    this.update_mention(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ensure_folder_paths(&mut self, root: String, cx: &mut Context<Self>) {
        if self.folder_paths.contains_key(&root) || !self.folder_paths_loading.insert(root.clone()) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let paths = client.list_paths(&root).await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.folder_paths_loading.remove(&root);
                this.folder_paths.insert(root, paths);
                cx.notify();
            });
        })
        .detach();
    }

    /// The `@` menu's rows right now.
    fn mention_matches(&self) -> Vec<MentionItem> {
        self.mention.as_ref().map(|m| self.mention_matches_for(m)).unwrap_or_default()
    }

    /// Up/down in the open `@` menu. False when it is closed.
    pub(super) fn mention_step(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
        let count = self.mention_matches().len();
        let Some(m) = self.mention.as_mut() else {
            return false;
        };
        if count > 0 {
            m.active = if down { (m.active + 1) % count } else { (m.active + count - 1) % count };
        }
        cx.notify();
        true
    }

    /// Take `item` from the `@` / `/` menu: what was typed is cleared and the
    /// item becomes a card in the input's row. With `drill` (Tab) a folder
    /// instead fills in its path and keeps the menu open, to go deeper.
    fn accept_mention(&mut self, item: Option<MentionItem>, drill: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(m) = self.mention.take() else {
            return false;
        };
        let Some(item) = item.or_else(|| self.mention_matches_for(&m).into_iter().nth(m.active)) else {
            return true;
        };
        let text = match item {
            MentionItem::Path(path) if drill && path.ends_with('/') => format!("@{path}"),
            // One command per message, first among the cards: it leads the
            // message on send, where the CLI expands it.
            MentionItem::Command(command) => {
                self.pending_refs.retain(|r| !matches!(r, MentionItem::Command(_)));
                self.refs_leaving.remove(&command.insert);
                self.pending_refs.insert(0, MentionItem::Command(command));
                String::new()
            }
            item => {
                // Picking one that is folding away brings it back instead.
                if self.refs_leaving.remove(&item.key()).is_none() && !self.pending_refs.contains(&item) {
                    self.pending_refs.push(item);
                }
                String::new()
            }
        };
        self.composer.update(cx, |input, cx| {
            let cursor = input.cursor();
            let value = input.value().to_string();
            // Taking a `/word` or `@word` out of the middle of a sentence:
            // take one of the spaces around it too, or two are left touching.
            let mut start = m.at;
            if text.is_empty() && value[..start].ends_with(' ') && (cursor == value.len() || value[cursor..].starts_with(' ')) {
                start -= 1;
            }
            input.set_selected_range(start..cursor, cx);
            input.replace(text, window, cx);
        });
        self.update_mention(cx);
        cx.notify();
        true
    }

    /// Picked references that aren't folding away: what a send includes.
    fn live_refs(&self) -> impl Iterator<Item = &MentionItem> {
        self.pending_refs.iter().filter(|r| !self.refs_leaving.contains_key(&r.key()))
    }

    /// A card's ×: it folds away in place, then leaves the list.
    fn remove_ref(&mut self, item: MentionItem, cx: &mut Context<Self>) {
        self.refs_gen += 1;
        let (n, key) = (self.refs_gen, item.key());
        self.refs_leaving.insert(key.clone(), n);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::motion::SLOW + Duration::from_millis(20)).await;
            let _ = this.update(cx, |this, cx| {
                if this.refs_leaving.get(&key) == Some(&n) {
                    this.refs_leaving.remove(&key);
                    this.pending_refs.retain(|r| r.key() != key);
                    this.chip_widths.borrow_mut().remove(&key);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// The picked repo's matching issues (none without a repo), then files.
    fn mention_matches_for(&self, m: &Mention) -> Vec<MentionItem> {
        if m.slash {
            let commands = self.commands.get(&self.commands_key()).map(Vec::as_slice).unwrap_or_default();
            let keys: Vec<(&str, &str)> = commands.iter().map(|c| (c.name.as_str(), c.description.as_str())).collect();
            return crate::mention::rank_commands(&keys, &m.query, MENTION_COMMAND_ROWS)
                .into_iter()
                .map(|ix| MentionItem::Command(commands[ix].clone()))
                .collect();
        }
        let issues = self.repo_issues.as_deref().filter(|_| self.issue_repo.is_some()).unwrap_or_default();
        let keys: Vec<(u64, &str)> = issues.iter().map(|i| (i.iid, i.title.as_str())).collect();
        let mut items: Vec<MentionItem> = crate::mention::rank_issues(&keys, &m.query, MENTION_ISSUE_ROWS)
            .into_iter()
            .map(|ix| MentionItem::Issue(issues[ix].clone()))
            .collect();
        let cwd = self.mention_root();
        if let Some(paths) = self.projects.mention_paths(self.current_chat.as_deref(), cwd.as_deref(), &self.folder_paths) {
            items.extend(crate::mention::rank(&paths, &m.query, MENTION_ROWS).into_iter().map(|p| MentionItem::Path(p.to_string())));
        } else if let Some(paths) = cwd.as_ref().and_then(|r| self.folder_paths.get(r)) {
            items.extend(crate::mention::rank(paths, &m.query, MENTION_ROWS).into_iter().map(|p| MentionItem::Path(p.to_string())));
        }
        items
    }

    fn active_status(&self) -> Option<ChatStatus> {
        self.current_chat
            .as_ref()
            .and_then(|id| self.state.chats.get(id))
            .map(|c| c.status)
    }

    pub fn handle_primary_button(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live: Vec<MentionItem> = self.live_refs().cloned().collect();
        let text = with_refs(self.composer.read(cx).value().trim(), &live);
        if self.attaching > 0 {
            return; // an image is still being stored; sending now would drop it
        }
        let has_images = !self.pending_images.is_empty();
        let Some(chat_id) = self.current_chat.clone() else {
            // No chat yet: create one and send this as its first message.
            if !text.is_empty() || has_images {
                self.clear_composer(window, cx);
                self.create_new_session(Some(text), cx);
            }
            return;
        };
        let is_running = self.active_status() == Some(ChatStatus::Running);
        // A chat keeps its harness; picking another one and sending starts a
        // new chat with it.
        let chat_harness = self.state.chats.get(&chat_id).map(|c| c.harness.clone()).unwrap_or_default();
        if !is_running
            && (!text.is_empty() || has_images)
            && !chat_harness.is_empty()
            && !self.selected_harness.is_empty()
            && chat_harness != self.selected_harness
        {
            self.clear_composer(window, cx);
            self.create_new_session(Some(text), cx);
            cx.notify();
            return;
        }
        match compute_button_action(is_running, &text) {
            PrimaryButtonAction::Send if text.is_empty() && !has_images => {}
            PrimaryButtonAction::Steer if text.is_empty() => {}
            PrimaryButtonAction::Send => {
                self.clear_composer(window, cx);
                self.send_text(&chat_id, text, false, cx);
            }
            PrimaryButtonAction::Steer => {
                self.clear_composer(window, cx);
                self.send_text(&chat_id, text, true, cx);
            }
            PrimaryButtonAction::Stop => self.handle_interrupt(cx),
        }
        cx.notify();
    }

    fn clear_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        // The cards fold away, as with each one's ×.
        for item in self.live_refs().cloned().collect::<Vec<_>>() {
            self.remove_ref(item, cx);
        }
        self.draft_save = None;
        if let Some(chat) = self.current_chat.clone() {
            self.state.set_draft(&chat, "");
        }
    }

    /// Echo the user's message locally (same message id the engine persists),
    /// re-engage stick-to-bottom, and queue the command.
    fn send_text(&mut self, chat_id: &str, text: String, steer: bool, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.invalidate_suggestion();
        let message_id = uuid::Uuid::new_v4().to_string();
        // Images go with a Send, never a steer.
        let images = if steer { Vec::new() } else { std::mem::take(&mut self.pending_images) };
        self.close_images_row(images.clone(), cx);
        let entry = self.message_entry(&message_id, "user");
        entry.blocks.set_text(&text);
        entry.images = images
            .iter()
            .map(|p| ImageRef { name: p.attachment.name.clone(), path: Some(p.path.to_string_lossy().into_owned()) })
            .collect();
        let attachments: Vec<keel_proto::Attachment> = images.into_iter().map(|p| p.attachment).collect();
        self.publish_rows(&message_id);
        self.mark_dirty(message_id.clone(), cx);
        self.transcript.on_own_send();
        if !steer {
            self.awaiting_first_output = true;
            self.loader_verbs = main_pane::THINKING_VERBS;
            self.loader_seed = (std::time::Instant::now(), uuid::Uuid::new_v4().as_u128() as usize);
        }

        let cmd = if steer {
            keel_proto::UserCommand::Steer { command_id: new_command_id(), message_id, text }
        } else {
            keel_proto::UserCommand::Send { command_id: new_command_id(), message_id, text, attachments }
        };
        let chat_id = chat_id.to_string();
        cx.spawn(async move |this, cx| {
            if let Err(err) = client.queue_command(&chat_id, cmd).await {
                let _ = this.update(cx, |this, cx| {
                    this.report("send failed", err);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The row id (turn head) whose choice buttons click: the latest turn,
    /// when it is an assistant turn and the chat is idle. A turn's rows live
    /// under its first message, not its last.
    pub(super) fn live_choices(&self) -> Option<String> {
        if matches!(self.active_status(), Some(ChatStatus::Running | ChatStatus::AwaitingInput)) {
            return None;
        }
        let last = self.msg_order.len().checked_sub(1)?;
        let assistant = |m: &String| self.messages.get(m).is_some_and(|e| e.role == "assistant");
        if !assistant(&self.msg_order[last]) {
            return None;
        }
        let (start, _) = turn_span(&self.msg_order, last, assistant);
        Some(self.msg_order[start].clone())
    }

    /// A choice button: sends its option as the next message, if the chat is
    /// still idle (a click can race a turn the user started another way).
    pub(super) fn choose(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(chat_id) = self.current_chat.clone() else {
            return;
        };
        if matches!(self.active_status(), Some(ChatStatus::Running | ChatStatus::AwaitingInput)) {
            return;
        }
        self.send_text(&chat_id, text, false, cx);
        cx.notify();
    }

    /// A code block's Run: the command in a new terminal tab in the chat's
    /// folder. A risky one is armed by the first click and runs on a second
    /// within a few seconds.
    pub(super) fn run_command(&mut self, row_id: String, command: String, risky: bool, window: &mut Window, cx: &mut Context<Self>) {
        const ARMED_FOR: std::time::Duration = std::time::Duration::from_secs(4);
        if risky && !self.armed_runs.contains(&row_id) {
            self.armed_runs.insert(row_id.clone());
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ARMED_FOR).await;
                let _ = this.update(cx, |this, cx| {
                    if this.armed_runs.remove(&row_id) {
                        cx.notify();
                    }
                });
            })
            .detach();
            cx.notify();
            return;
        }
        self.armed_runs.remove(&row_id);
        self.run_in_terminal(command, window, cx);
        cx.notify();
    }

    /// A diagram node: the composer starts a question about it, for the user
    /// to finish. Nothing is sent.
    pub(super) fn ask_about(&mut self, label: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = format!("About \"{label}\": ");
        self.composer.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// An MR card's Merge button. The first click arms it for a few seconds;
    /// a second one asks the engine to merge, which checks the MR live and
    /// rewrites the card (merged) on success.
    pub(super) fn merge_mr(&mut self, row_id: String, message_id: String, part_id: String, cx: &mut Context<Self>) {
        use crate::transcript::MergeUi;
        const ARMED_FOR: std::time::Duration = std::time::Duration::from_secs(4);
        let (Some(chat_id), Some(client)) = (self.current_chat.clone(), self.client.clone()) else {
            return;
        };
        match self.mr_merges.get(&row_id) {
            Some(MergeUi::Merging) => return,
            Some(MergeUi::Armed) => {
                self.mr_merges.insert(row_id.clone(), MergeUi::Merging);
                cx.spawn(async move |this, cx| {
                    let params = serde_json::json!({ "chat_id": chat_id, "message_id": message_id, "part_id": part_id });
                    let res = client.call("merge_mr", params).await;
                    let _ = this.update(cx, |this, cx| {
                        match res {
                            Ok(_) => {
                                this.mr_merges.remove(&row_id);
                                this.mr_poke(cx);
                            }
                            Err(err) => {
                                this.mr_merges.insert(row_id, MergeUi::Failed(err.to_string()));
                            }
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            None | Some(MergeUi::Failed(_)) => {
                self.mr_merges.insert(row_id.clone(), MergeUi::Armed);
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(ARMED_FOR).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.mr_merges.get(&row_id) == Some(&MergeUi::Armed) {
                            this.mr_merges.remove(&row_id);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    pub fn handle_interrupt(&mut self, cx: &mut Context<Self>) {
        let (Some(chat_id), Some(client)) = (self.current_chat.clone(), self.client.clone()) else {
            return;
        };
        if let Some(panel) = self.question_panels.remove(&chat_id) {
            self.question_stopped.insert(chat_id.clone());
            if let Some(receipt) = panel.current_question().map(|q| keel_proto::Receipt::ended(q, keel_proto::Decision::Stopped)) {
                self.begin_question_exit(chat_id.clone(), panel, None, receipt, true, cx);
            }
        }
        let cmd = keel_proto::UserCommand::Interrupt { command_id: new_command_id() };
        cx.spawn(async move |_, _| {
            let _ = client.queue_command(&chat_id, cmd).await;
        })
        .detach();
        cx.notify();
    }

    // ── Questions ───────────────────────────────────────────────────────

    /// Send the answers and let the card fold into its receipt. `chosen` is
    /// the button or row that was used, for its check.
    fn finish_question(&mut self, question_id: String, answers: Vec<keel_proto::UserInputAnswer>, chosen: Option<usize>, cx: &mut Context<Self>) {
        let (Some(chat_id), Some(client)) = (self.current_chat.clone(), self.client.clone()) else {
            return;
        };
        if let Some(panel) = self.question_panels.remove(&chat_id) {
            let receipt = panel
                .current_question()
                .zip(answers.last())
                .map(|(q, a)| keel_proto::Receipt::for_answer(q, a));
            if let Some(receipt) = receipt {
                self.begin_question_exit(chat_id.clone(), panel, chosen, receipt, false, cx);
            }
        }
        let cmd = keel_proto::UserCommand::Answer { command_id: new_command_id(), question_id, answers };
        cx.spawn(async move |_, _| {
            let _ = client.queue_command(&chat_id, cmd).await;
        })
        .detach();
        cx.notify();
    }

    fn with_panel<R>(&mut self, f: impl FnOnce(&mut QuestionPanelState) -> R) -> Option<(String, R)> {
        let chat = self.current_chat.clone()?;
        let panel = self.question_panels.get_mut(&chat)?;
        let out = f(panel);
        Some((panel.question_id.clone(), out))
    }

    pub(super) fn current_panel(&self) -> Option<&QuestionPanelState> {
        self.current_chat.as_ref().and_then(|c| self.question_panels.get(c))
    }

    /// Answer with option `ix` of the current question; `chosen` is the
    /// button or row that shows the check.
    pub(super) fn answer_option(&mut self, ix: usize, chosen: Option<usize>, cx: &mut Context<Self>) {
        if let Some((qid, Some(answers))) = self.with_panel(|p| p.pick(ix)) {
            self.finish_question(qid, answers, chosen, cx);
        }
        self.focus_question = true;
        cx.notify();
    }

    pub fn handle_question_answer(&mut self, digit: usize, cx: &mut Context<Self>) {
        if let Some(ix) = digit.checked_sub(1) {
            self.answer_option(ix, Some(ix), cx);
        }
    }

    pub(super) fn submit_checked(&mut self, cx: &mut Context<Self>) {
        if let Some((qid, Some(answers))) = self.with_panel(|p| p.submit_checked()) {
            self.finish_question(qid, answers, None, cx);
        }
        self.focus_question = true;
        cx.notify();
    }

    pub(super) fn open_other(&mut self, cx: &mut Context<Self>) {
        self.with_panel(|p| {
            p.other_open = true;
            p.focus = p.current_question().map_or(0, |q| q.options.len());
        });
        self.focus_question = true;
        cx.notify();
    }

    fn skip_question(&mut self, cx: &mut Context<Self>) {
        if let Some((qid, Some(answers))) = self.with_panel(|p| p.escape()) {
            self.finish_question(qid, answers, None, cx);
        }
        self.focus_question = true;
        cx.notify();
    }

    fn submit_question_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.question_input.read(cx).value().to_string();
        if text.trim().is_empty() && self.current_panel().is_some_and(|p| p.other_open) {
            return;
        }
        let Some((qid, done)) = self.with_panel(|p| {
            if p.needs_text() { Some(p.submit_text(&text)) } else { None }
        }) else {
            return;
        };
        let Some(done) = done else {
            return;
        };
        self.question_input.update(cx, |s, cx| s.set_value("", window, cx));
        if let Some(answers) = done {
            self.finish_question(qid, answers, None, cx);
        }
        self.focus_question = true;
        cx.notify();
    }

    /// Card keys. Permission: 1/2/3 left to right (Allow, Allow always,
    /// Deny), Enter = focused (Allow), Cmd+Enter = Allow always, Esc = Deny,
    /// Tab/arrows move the focus. Question: digits pick (multi: toggle),
    /// O = Other…, Enter chooses, Esc skips an optional one. Cmd+. stops.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        use crate::approval::{CardKey, parse_key, permission_buttons, permission_key, question_key};
        let Some(chat) = self.current_chat.clone() else {
            return;
        };
        let Some(panel) = self.question_panels.get(&chat).cloned() else {
            // Keys landing while a card folds away must not reach anything else.
            if self.question_exit_showing() && matches!(event.keystroke.key.as_str(), "enter" | "escape" | "1" | "2" | "3") {
                cx.stop_propagation();
            }
            return;
        };
        let Some(question) = panel.current_question().cloned() else {
            return;
        };
        let ks = &event.keystroke;
        let m = ks.modifiers;
        if m.platform && ks.key == "." {
            cx.stop_propagation();
            self.handle_interrupt(cx);
            return;
        }
        if panel.needs_text() {
            // The field takes the keys; Escape closes "Other…" or skips.
            if ks.key == "escape" {
                cx.stop_propagation();
                if panel.other_open {
                    self.with_panel(|p| p.other_open = false);
                    self.focus_question = true;
                    window.focus(&self.focus_handle, cx);
                    cx.notify();
                } else {
                    self.skip_question(cx);
                }
            }
            return;
        }
        if m.control || m.alt || m.function || (m.shift && ks.key != "tab") || (m.platform && ks.key != "enter") {
            return;
        }
        let Some(key) = parse_key(&ks.key, m.shift) else {
            return;
        };
        let action = if question.is_permission() {
            let buttons = permission_buttons(&question);
            permission_key(&buttons, panel.focus, key, m.platform).map(|a| match a {
                // Buttons index into the role-ordered row; answer with the option behind it.
                CardKey::Pick(b) => (CardKey::Pick(buttons[b].option), Some(b)),
                other => (other, None),
            })
        } else {
            question_key(&question, panel.focus, panel.checked.len(), key, m.platform).map(|a| (a, None))
        };
        let Some((action, button)) = action else {
            return;
        };
        cx.stop_propagation();
        match action {
            CardKey::Pick(option) => {
                let chosen = button.or(Some(option));
                self.with_panel(|p| p.keyboard = true);
                self.answer_option(option, chosen, cx);
            }
            CardKey::Focus(ix) => {
                self.with_panel(|p| {
                    p.focus = ix;
                    p.keyboard = true;
                });
                cx.notify();
            }
            CardKey::Toggle(ix) => {
                self.with_panel(|p| {
                    p.toggle(ix);
                    p.focus = ix;
                });
                cx.notify();
            }
            CardKey::Submit => self.submit_checked(cx),
            CardKey::OpenOther => self.open_other(cx),
            CardKey::Skip => self.skip_question(cx),
        }
    }

    // ── Sessions, spaces, chats ─────────────────────────────────────────

    /// "+" / ⌘N: the new-chat screen. The chat itself is created on the
    /// first send, in the folder picked there (or the filtered / newest
    /// space); with no space at all, in "No project".
    pub fn create_new_session(&mut self, first_message: Option<String>, cx: &mut Context<Self>) {
        self.close_menus(cx);
        match (self.state.target_space_for_new_session(), first_message) {
            (Some(space), Some(text)) => self.create_chat_in(space.id, space.path, Some(text), cx),
            (None, Some(text)) => self.create_no_project_chat(text, cx),
            (_, None) => {
                self.close_pages();
                self.finish_settings_exit(cx);
                self.state.active_tab = None;
                self.card_sync_chat(cx);
                cx.notify();
            }
        }
    }

    /// Leave Settings / GitLab for the chat area. Settings stays painted
    /// for one leave so it can exit; GitLab cuts.
    pub(super) fn close_pages(&mut self) {
        self.begin_settings_exit();
        self.gitlab_open = false;
        self.projects.close_home();
        self.projects.folder_card = None;
    }

    pub(super) fn begin_settings_exit(&mut self) {
        if !self.settings_open {
            return;
        }
        self.settings_open = false;
        if self.reduced() {
            self.settings_leaving = None;
            return;
        }
        self.settings_gen += 1;
        self.settings_leaving = Some(self.settings_gen);
    }

    /// Unmount a settings exit once its sheet-close has had time to play.
    /// Call once per close, from the site that has a context.
    pub(super) fn finish_settings_exit(&mut self, cx: &mut Context<Self>) {
        let Some(n) = self.settings_leaving else { return };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::motion::BASE + std::time::Duration::from_millis(20)).await;
            let _ = this.update(cx, |this, cx| this.clear_settings_exit(n, cx));
        })
        .detach();
    }

    /// Drop a finished settings exit, unless a newer one replaced it.
    fn clear_settings_exit(&mut self, n: u64, cx: &mut Context<Self>) {
        if self.settings_leaving == Some(n) {
            self.settings_leaving = None;
            cx.notify();
        }
    }

    /// Folder for the next chat, from the new-chat screen: no chat is created.
    pub(super) fn pick_folder_for_new_chat(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.use_folder_for_new_chat(path.to_string_lossy().into_owned(), cx));
        })
        .detach();
    }

    /// "No project": an empty folder of Keel's own, named so its space reads
    /// "No project".
    pub(super) fn use_no_project_for_new_chat(&mut self, cx: &mut Context<Self>) {
        let dir = self.state.data_dir.join("No project");
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.report("No project folder", err.to_string());
            return;
        }
        self.use_folder_for_new_chat(dir.to_string_lossy().into_owned(), cx);
    }

    /// Make `path` (a space, created or reused) the next chat's folder.
    fn use_folder_for_new_chat(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.report("no engine", "not connected");
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.call("create_space", serde_json::json!({ "path": path })).await;
            let _ = this.update(cx, |this, cx| {
                match res.as_ref().ok().and_then(|v| v.get("space_id")).and_then(|s| s.as_str()) {
                    Some(space_id) => {
                        this.state.record_created_space(space_id, &path);
                        this.state.new_chat_space = Some(space_id.to_string());
                        this.state.reset_branch_for_folder();
                        this.create_error = None;
                    }
                    None => this.report("create_space failed", res.err().unwrap_or_else(|| "no space_id".into())),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// First send with no folder yet: creates (or reuses) the "No project"
    /// space and starts the chat there.
    fn create_no_project_chat(&mut self, first_message: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.report("no engine", "not connected");
            return;
        };
        let dir = self.state.data_dir.join("No project");
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.report("No project folder", err.to_string());
            return;
        }
        let path = dir.to_string_lossy().into_owned();
        cx.spawn(async move |this, cx| {
            let res = client.call("create_space", serde_json::json!({ "path": path })).await;
            let _ = this.update(cx, |this, cx| {
                match res.and_then(|v| {
                    v.get("space_id")
                        .and_then(|s| s.as_str())
                        .map(String::from)
                        .ok_or_else(|| "create_space returned no space_id".to_string())
                }) {
                    Ok(space_id) => {
                        this.state.record_created_space(&space_id, &path);
                        this.create_chat_in(space_id, path, Some(first_message), cx);
                    }
                    Err(err) => this.report("create_space failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create_chat_in(&mut self, space_id: String, cwd: String, first_message: Option<String>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let harness = self.selected_harness.clone();
        let model = self.selected_model.clone();
        let effort = self.selected_effort.clone();
        let permission = self.effective_permission().as_str();
        self.creating_chat = first_message.is_some();
        self.create_error = None;
        // Link the chat to its project before the first message can start a run.
        let link = self.projects.link_plan(&cwd);
        let params = serde_json::json!({
            "space_id": space_id,
            "cwd": cwd,
            "title": "New Chat",
            "harness": harness,
            "model": model,
            "reasoning": effort,
            "permission": permission,
            "branch": self.state.selected_branch,
        });
        cx.spawn(async move |this, cx| {
            let res = client.call("create_chat", params).await;
            let _ = this.update(cx, |this, cx| {
                this.creating_chat = false;
                let chat_id = res.and_then(|v| {
                    v.get("chat_id")
                        .and_then(|s| s.as_str())
                        .map(String::from)
                        .ok_or_else(|| "create_chat returned no chat_id".to_string())
                });
                match chat_id {
                    Ok(chat_id) => {
                        let now = AppState::now_ms();
                        let chat = this.state.chats.entry(chat_id.clone()).or_insert_with(|| {
                            ChatItem::test(&chat_id, &space_id, "New Chat", ChatStatus::Idle, now, now)
                        });
                        chat.harness = harness;
                        chat.model = model;
                        chat.reasoning = effort;
                        chat.reasoning_known = true;
                        chat.permission = permission.to_string();
                        if let Some((project, scope)) = link {
                            this.projects.record_chat(&chat_id, &project, scope);
                        }
                        if let Some(text) = first_message {
                            this.pending_first_message = Some((chat_id.clone(), text));
                        }
                        this.state.open_or_focus_tab(&chat_id);
                    }
                    Err(err) => {
                        // Shown at the composer (e.g. a dirty tree refusing a
                        // branch switch); the message goes back in the input.
                        tracing::warn!("create_chat failed: {err}");
                        this.create_error = Some(err);
                        this.restore_draft = first_message;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn archive_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            if let Some(chat) = self.state.chats.get_mut(&chat_id) {
                chat.archived = true;
            }
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.set_archived(&chat_id, true).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(()) => {
                        if let Some(chat) = this.state.chats.get_mut(&chat_id) {
                            chat.archived = true;
                        }
                    }
                    Err(err) => this.report("archive failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The pointer entered or left the toggle or the floating sidebar. Entering
    /// shows it; leaving closes it after a beat, so crossing the gap between
    /// the button and the panel doesn't flicker it shut.
    pub(super) fn peek_hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if !self.state.ui_settings.sidebar_collapsed {
            return;
        }
        if hovered {
            self.peek_close = None;
            if !self.sidebar_peek {
                // Also catches it mid-exit: it slides back in.
                self.sidebar_peek = true;
                self.peek_closing = false;
                self.peek_gen += 1;
                cx.notify();
            }
            return;
        }
        self.peek_close = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(220)).await;
            let _ = this.update(cx, |this, cx| {
                this.peek_close = None;
                // A menu or prompt opened from the panel keeps it up.
                let busy = this.state.space_menu.is_some()
                    || this.state.show_overflow_menu
                    || this.confirming_delete_chats.is_some()
                    || this.delete_confirm_leaving.is_some()
                    || this.state.confirming_delete_space.is_some()
                    || this.state.renaming_space.is_some();
                if busy || !this.sidebar_peek {
                    return;
                }
                // Slide out, then stop drawing it once the exit has played.
                this.sidebar_peek = false;
                this.peek_closing = true;
                this.peek_gen += 1;
                cx.notify();
                let generation = this.peek_gen;
                this.peek_close = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(crate::motion::FAST + Duration::from_millis(30)).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.peek_gen == generation {
                            this.peek_closing = false;
                            cx.notify();
                        }
                    });
                }));
            });
        }));
    }

    /// Delete chats everywhere. Deleting the open chat lands on the new-chat
    /// screen rather than whichever chat happens to be next.
    pub fn delete_chats(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        self.confirming_delete_chats = None;
        self.delete_confirm_leaving = None;
        // Play the row leave first, then hit the engine.
        self.rows_exit_gen += 1;
        self.rows_exiting = ids.clone();
        let Some(client) = self.client.clone() else {
            self.report("delete failed", "not connected");
            self.rows_exiting.clear();
            return;
        };
        let exit_gen = self.rows_exit_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::motion::BASE).await;
            let res = client.delete_chats(&ids).await;
            let _ = this.update(cx, |this, cx| {
                if this.rows_exit_gen == exit_gen {
                    this.rows_exiting.clear();
                }
                match res {
                    Ok(()) => {
                        let active_deleted = this.state.active_tab.as_ref().is_some_and(|a| ids.contains(a));
                        for id in &ids {
                            if let Some(chat) = this.state.chats.get_mut(id) {
                                chat.tombstoned = true;
                            }
                            this.state.close_tab(id);
                        }
                        if active_deleted {
                            this.state.active_tab = None;
                        }
                        this.selected_chats.retain(|id| !ids.contains(id));
                        if this.selected_chats.is_empty() {
                            this.select_anchor = None;
                            this.select_leaving = None;
                        }
                    }
                    Err(err) => this.report("delete failed", err),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Enter select mode (first pick) or clear it with an exit fade.
    pub(super) fn note_selection_changed(&mut self, had: usize, _cx: &mut Context<Self>) {
        let now = self.selected_chats.len();
        if had == 0 && now > 0 {
            self.select_mode_gen += 1;
            self.select_leaving = None;
        } else if had > 0 && now == 0 && self.select_leaving.is_none() {
            // Cleared instantly (e.g. after delete emptied the set): nothing to fade.
        }
    }

    /// Open the centered delete confirmation for these chat ids.
    pub(super) fn ask_delete_chats(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        self.delete_confirm_gen += 1;
        self.delete_confirm_leaving = None;
        self.confirming_delete_chats = Some(ids);
        cx.notify();
    }

    /// Fade the delete modal out (Cancel, Escape, backdrop). Keeps the card
    /// on screen for one FAST beat so the exit plays.
    pub(super) fn dismiss_delete_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(ids) = self.confirming_delete_chats.take() else {
            return;
        };
        self.delete_confirm_gen += 1;
        let n = self.delete_confirm_gen;
        self.delete_confirm_leaving = Some((n, ids));
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::motion::FAST + std::time::Duration::from_millis(20))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.delete_confirm_leaving.as_ref().is_some_and(|(m, _)| *m == n) {
                    this.delete_confirm_leaving = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn enter_select_mode(&mut self, cx: &mut Context<Self>) {
        if self.select_mode {
            return;
        }
        self.summary_hover.clear();
        self.select_mode = true;
        self.select_mode_gen += 1;
        self.select_leaving = None;
        cx.notify();
    }

    pub(super) fn exit_select_mode(&mut self, cx: &mut Context<Self>) {
        self.select_mode = false;
        self.fade_out_selection(cx);
    }

    pub(super) fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.select_mode = false;
        if self.selected_chats.is_empty() {
            return;
        }
        self.fade_out_selection(cx);
    }

    fn fade_out_selection(&mut self, cx: &mut Context<Self>) {
        self.select_leaving = Some(self.select_mode_gen);
        let n = self.select_mode_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::motion::FAST + std::time::Duration::from_millis(20))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.select_leaving == Some(n) {
                    this.selected_chats.clear();
                    this.select_anchor = None;
                    this.select_leaving = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn delete_space(&mut self, space_id: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.state.on_space_deleted(&space_id);
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.delete_space(&space_id).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(()) => this.state.on_space_deleted(&space_id),
                    Err(err) => this.report("delete space failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn start_rename(&mut self, space_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.state.spaces.get(&space_id).map(|s| s.name.clone()).unwrap_or_default();
        self.rename_input.update(cx, |s, cx| {
            s.set_value(name.clone(), window, cx);
            s.focus(window, cx);
        });
        self.state.renaming_space = Some((space_id, name));
        self.state.space_menu = None;
        cx.notify();
    }

    fn save_rename(&mut self, cx: &mut Context<Self>) {
        let Some((space_id, _)) = self.state.renaming_space.clone() else {
            return;
        };
        let name = self.rename_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.rename_space(space_id, name, cx);
    }

    pub fn rename_space(&mut self, space_id: String, new_name: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            if let Some(space) = self.state.spaces.get_mut(&space_id) {
                space.name = new_name;
            }
            self.state.renaming_space = None;
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.rename_space(&space_id, &new_name).await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(()) => {
                        if let Some(space) = this.state.spaces.get_mut(&space_id) {
                            space.name = new_name;
                        }
                        this.state.renaming_space = None;
                    }
                    Err(err) => this.report("rename failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ── Pickers ─────────────────────────────────────────────────────────

    fn load_harnesses(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let res = client.list_harnesses().await;
            let _ = this.update(cx, |this, cx| {
                match res {
                    Ok(rows) => {
                        this.harnesses = HarnessOption::parse_list(&rows);
                        let selectable = this
                            .harnesses
                            .iter()
                            .any(|h| h.installed && h.id == this.selected_harness);
                        if !selectable && let Some(id) = HarnessOption::default_id(&this.harnesses) {
                            this.selected_harness = id;
                        }
                        if this.current_chat.is_none() {
                            this.apply_harness_prefs();
                        }
                        let harness = this.selected_harness.clone();
                        if !harness.is_empty() {
                            this.ensure_models(harness, cx);
                        }
                    }
                    Err(err) => this.report("list_harnesses failed", err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn ensure_models(&mut self, harness: String, cx: &mut Context<Self>) {
        if self.models.contains_key(&harness) || !self.models_loading.insert(harness.clone()) {
            return;
        }
        self.models_error.remove(&harness);
        let Some(client) = self.client.clone() else {
            return;
        };
        // The skeleton shimmers on the shared ticker while this loads.
        self.kick_motion(cx);
        cx.spawn(async move |this, cx| {
            let res = client.call("list_models", serde_json::json!({ "harness": harness })).await;
            let _ = this.update(cx, |this, cx| {
                this.models_loading.remove(&harness);
                match res {
                    Ok(v) => {
                        this.models_ready_at.insert(harness.clone(), std::time::Instant::now());
                        this.kick_motion(cx);
                        let list = ModelList::parse(&v);
                        if harness == this.selected_harness
                            && (this.selected_model.is_empty() || list.get(&this.selected_model).is_none())
                        {
                            this.selected_model = list.initial_model();
                        }
                        if harness == this.selected_harness {
                            this.selected_effort = list.coerce_effort(&this.selected_model, &this.selected_effort);
                        }
                        let current = harness == this.selected_harness;
                        this.models.insert(harness, list);
                        if current {
                            this.remember_harness_prefs();
                        }
                    }
                    Err(err) => {
                        this.models_error.insert(harness, err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn select_harness(&mut self, id: String, cx: &mut Context<Self>) {
        // A chat keeps the harness it started with.
        if self.current_chat.is_some() {
            return;
        }
        let before = self.model_label();
        let permission = self.effective_permission();
        self.selected_model = self.models.get(&id).map(|m| m.initial_model()).unwrap_or_default();
        self.selected_effort.clear();
        self.selected_harness = id.clone();
        self.apply_harness_prefs();
        if self.effective_permission() != permission {
            self.permission_prev = permission;
            self.permission_roll += 1;
        }
        self.show_harness_picker = false;
        self.start_model_roll(before, true);
        self.ensure_models(id, cx);
        cx.notify();
    }

    fn select_model(&mut self, model: String, cx: &mut Context<Self>) {
        let before = self.model_label();
        let up = self
            .models
            .get(&self.selected_harness)
            .map(|l| {
                let at = |id: &str| l.models.iter().position(|m| m.id == id);
                at(&model) >= at(&self.selected_model)
            })
            .unwrap_or(true);
        self.selected_effort = self
            .models
            .get(&self.selected_harness)
            .map(|l| l.coerce_effort(&model, &self.selected_effort))
            .unwrap_or_default();
        self.selected_model = model;
        self.show_model_picker = false;
        self.start_model_roll(before, up);
        self.remember_harness_prefs();
        self.push_chat_options(cx);
        cx.notify();
    }

    fn select_effort(&mut self, effort: String, cx: &mut Context<Self>) {
        if effort == self.selected_effort {
            return;
        }
        // The knob glides from where it was to the new stop.
        self.effort_from = self.effort_fraction();
        self.effort_glide += 1;
        self.effort_prev = std::mem::replace(&mut self.selected_effort, effort);
        self.remember_harness_prefs();
        self.push_chat_options(cx);
        cx.notify();
    }

    /// Model + effort apply to the active chat's next turn (when it runs the
    /// selected harness), and to new sessions.
    fn push_chat_options(&mut self, cx: &mut Context<Self>) {
        let Some(chat_id) = self.current_chat.clone() else {
            return;
        };
        let Some(chat) = self.state.chats.get_mut(&chat_id) else {
            return;
        };
        if !chat.harness.is_empty() && chat.harness != self.selected_harness {
            return; // harness cannot change on an existing chat
        }
        chat.model = self.selected_model.clone();
        chat.reasoning = self.selected_effort.clone();
        chat.reasoning_known = true;
        let permission = self.effective_permission().as_str();
        if let Some(chat) = self.state.chats.get_mut(&chat_id) {
            chat.permission = permission.to_string();
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let params = serde_json::json!({
            "chat_id": chat_id,
            "model": self.selected_model,
            "reasoning": self.selected_effort,
            "permission": permission,
        });
        cx.spawn(async move |this, cx| {
            if let Err(err) = client.call("set_chat_options", params).await {
                let _ = this.update(cx, |this, cx| {
                    this.report("set_chat_options failed", err);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Rolls the model chip's label from `before` to the current label, if
    /// it changed: same motion as the effort name's roll.
    fn start_model_roll(&mut self, before: String, up: bool) {
        if before != self.model_label() {
            self.model_prev = before;
            self.model_roll += 1;
            self.model_roll_up = up;
        }
    }

    /// A model's display name, when its harness list or the saved pick knows it.
    fn model_name(&self, harness: &str, model: &str) -> Option<String> {
        match self.models.get(harness) {
            Some(l) => l.get(model).map(|m| m.name.clone()),
            // List still loading: the name saved with the pick, not the raw id.
            None => self
                .state
                .ui_settings
                .harness_prefs
                .get(harness)
                .filter(|p| p.model == model && !p.model_name.is_empty())
                .map(|p| p.model_name.clone()),
        }
    }

    fn model_label(&self) -> String {
        match self.model_name(&self.selected_harness, &self.selected_model) {
            Some(n) => n,
            None if self.selected_model.is_empty() => "default".into(),
            None => self.selected_model.clone(),
        }
    }

    fn harness_label(&self) -> String {
        self.harnesses
            .iter()
            .find(|h| h.id == self.selected_harness)
            .map(|h| h.display_name.clone())
            .unwrap_or_else(|| {
                if self.selected_harness.is_empty() {
                    "…".into()
                } else {
                    self.selected_harness.clone()
                }
            })
    }

    fn effort_levels(&self) -> Vec<String> {
        self.models
            .get(&self.selected_harness)
            .map(|l| l.levels(&self.selected_model).to_vec())
            .unwrap_or_default()
    }
}

// ── Shared widgets ──────────────────────────────────────────────────────

use gpui::{Anchor, AnyElement, ElementId, div, prelude::*};
use crate::icons::Icon;
use crate::style::{KeelStyled, RADIUS_MD, RADIUS_SM};

/// One row of a menu or picker: full-width, hover overlay, optional check.
fn menu_item(id: impl Into<gpui::SharedString>, t: Tokens, enabled: bool, checked: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .px_2()
        .py_1()
        .rounded(RADIUS_SM)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .t_body()
        .text_color(c(if enabled { t.text } else { t.text_faint }))
        .when(enabled, |s| s.cursor_pointer().hover(|s| s.bg(c(t.hover))))
        .child(
            div()
                .w(zpx(14.))
                .flex_none()
                .text_color(c(t.accent))
                .when(checked, |d| d.child(Icon::Check.el(zpx(13.)))),
        )
}

/// A menu's empty-section placeholder: a muted icon and text on a
/// [`menu_item`]'s geometry (the icon in its check column, so the text lines
/// up with the rows'). Static: no id, hover or click, so keyboard and
/// pointer pass it by.
fn menu_empty_row(icon: Icon, text: impl Into<gpui::SharedString>, t: Tokens) -> gpui::Div {
    div()
        .px_2()
        .py_1()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .t_body()
        .text_color(c(t.text_faint))
        .child(div().w(zpx(14.)).flex_none().flex().justify_center().child(icon.el(zpx(13.))))
        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text.into()))
}

/// Section label inside a menu.
fn menu_label(text: impl Into<gpui::SharedString>, t: Tokens) -> gpui::Div {
    div()
        .px_2()
        .pt_1p5()
        .pb_0p5()
        .t_caption()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(c(t.text_faint))
        .child(text.into())
}

fn menu_separator(t: Tokens) -> gpui::Div {
    div().my_1().h(zpx(1.)).bg(c(t.border))
}

/// A floating menu anchored below its trigger. Clicking outside closes it;
/// `leaving` (from [`KeelWindow::menu_phase`]) plays its exit.
#[allow(dead_code)]
fn floating_menu(
    id: &'static str,
    t: Tokens,
    width: f32,
    leaving: Option<u64>,
    reduced: bool,
    on_dismiss: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
    items: Vec<AnyElement>,
) -> AnyElement {
    floating_menu_at(id, t, width, false, leaving, reduced, on_dismiss, items)
}

/// A floating menu on the sidebar, matching the sidebar's background color.
fn sidebar_menu(
    id: &'static str,
    t: Tokens,
    width: f32,
    leaving: Option<u64>,
    reduced: bool,
    on_dismiss: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
    items: Vec<AnyElement>,
) -> AnyElement {
    let anchor = Anchor::TopLeft;
    let panel = menu_panel_with_bg(id, t, width, false, c(t.sidebar), items).when(leaving.is_none(), |d| d.on_mouse_down_out(on_dismiss));
    anchored_menu_positioned(anchor, None, menu_motion(panel, id, false, leaving, reduced))
}

/// Like [`floating_menu`]; `up` opens it above the trigger (composer pickers).
#[allow(clippy::too_many_arguments)]
fn floating_menu_at(
    id: &'static str,
    t: Tokens,
    width: f32,
    up: bool,
    leaving: Option<u64>,
    reduced: bool,
    on_dismiss: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
    items: Vec<AnyElement>,
) -> AnyElement {
    let anchor = if up { Anchor::BottomLeft } else { Anchor::TopLeft };
    floating_menu_positioned(id, t, width, anchor, None, up, leaving, reduced, on_dismiss, items)
}

#[allow(clippy::too_many_arguments)]
fn floating_menu_positioned(
    id: &'static str,
    t: Tokens,
    width: f32,
    anchor: Anchor,
    at: Option<gpui::Point<gpui::Pixels>>,
    up: bool,
    leaving: Option<u64>,
    reduced: bool,
    on_dismiss: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
    items: Vec<AnyElement>,
) -> AnyElement {
    let panel = menu_panel(id, t, width, up, items).when(leaving.is_none(), |d| d.on_mouse_down_out(on_dismiss));
    anchored_menu_positioned(anchor, at, menu_motion(panel, id, up, leaving, reduced))
}

/// Every floating surface's motion. In: fades while drifting 4 px out from
/// its trigger's side. `leaving`: the entrance played backwards, drifting
/// back toward the trigger and quickening as it goes; `n` new per close.
fn menu_motion<E: IntoElement + Styled + 'static>(el: E, id: &str, up: bool, leaving: Option<u64>, reduced: bool) -> AnyElement {
    use gpui::AnimationExt as _;
    let rise = if up { 4. } else { -4. };
    match leaving {
        Some(_) if reduced => gpui::Empty.into_any_element(),
        None => crate::motion::appear(el, ElementId::Name(format!("{id}-appear").into()), crate::motion::FAST, rise, reduced).into_any_element(),
        Some(n) => el
            .with_animation(
                ElementId::Name(format!("{id}-leave-{n}").into()),
                gpui::Animation::new(crate::motion::FAST).with_easing(crate::motion::ease_in),
                move |el, delta| el.opacity(1. - delta).relative().top(zpx(rise * delta)),
            )
            .into_any_element(),
    }
}

/// The floating surface shared by menus: fixed width, scrolls past 360 px.
fn menu_panel(id: &'static str, t: Tokens, width: f32, up: bool, items: Vec<AnyElement>) -> gpui::Stateful<gpui::Div> {
    menu_panel_with_bg(id, t, width, up, c(t.floating), items)
}

/// A menu panel with a configurable background color.
fn menu_panel_with_bg(id: &'static str, t: Tokens, width: f32, up: bool, bg: gpui::Hsla, items: Vec<AnyElement>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .when(up, |d| d.mb_1())
        .when(!up, |d| d.mt_1())
        .w(zpx(width))
        .occlude()
        .max_h(zpx(360.))
        .overflow_y_scroll()
        .p_1()
        .flex()
        .flex_col()
        .floating_surface(t)
        .bg(bg)
        .children(items)
}

/// Above everything, beside its trigger, kept inside the window.
fn anchored_menu_positioned(anchor: Anchor, at: Option<gpui::Point<gpui::Pixels>>, menu: AnyElement) -> AnyElement {
    let mut a = gpui::anchored().anchor(anchor).snap_to_window_with_margin(zpx(8.));
    if let Some(pos) = at {
        a = a.position(pos);
    }
    gpui::deferred(a.child(menu))
        .with_priority(1)
        .into_any_element()
}

/// Small square button for header controls.
fn icon_button(id: &'static str, icon: Icon, t: Tokens) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(zpx(26.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_MD)
        .text_color(c(t.text_muted))
        .cursor_pointer()
        .hover(|s| s.bg(c(t.hover)).text_color(c(t.text)))
        .child(icon.el(zpx(15.)))
}

/// Empty title-bar space: drags the window, double-click zooms it.
fn drag_region(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .h_full()
        .on_mouse_down(gpui::MouseButton::Left, |event: &gpui::MouseDownEvent, window, _| {
            if event.click_count >= 2 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
        })
}

/// Run state as a dot: running accent (breathing), waiting warning, aborted
/// danger, idle none.
fn status_dot(status: keel_proto::DisplayStatus, id: &str, t: Tokens, reduced: bool) -> Option<AnyElement> {
    use keel_proto::DisplayStatus;
    let color = match status {
        DisplayStatus::Running => t.accent,
        DisplayStatus::AwaitingInput => t.warning,
        DisplayStatus::Aborted => t.danger,
        DisplayStatus::Unknown => t.text_faint,
        DisplayStatus::Idle => return None,
    };
    let dot = div().size(zpx(7.)).flex_none().rounded_full().bg(c(color));
    Some(match status {
        DisplayStatus::Running => crate::motion::pulse(dot, ElementId::Name(format!("pulse-{id}").into()), reduced),
        _ => dot.into_any_element(),
    })
}

/// Image by extension; the host checks the bytes again before storing.
fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp"))
}

/// The `@` reference being typed in the composer.
#[derive(Clone, Debug)]
pub(super) struct Mention {
    pub at: usize,
    pub query: String,
    pub active: usize,
    /// A `/` command at the start of the message, rather than an `@` reference.
    pub slash: bool,
}

/// A slash command or skill the chat's harness picks up from disk.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SlashCommand {
    pub name: String,
    /// What invokes it, put at the start of the message: `/review`.
    pub insert: String,
    pub description: String,
    pub skill: bool,
    pub project: bool,
}

impl SlashCommand {
    fn parse(v: &serde_json::Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self { name: s("name"), insert: s("insert"), description: s("description"), skill: s("kind") == "skill", project: s("scope") == "project" }
    }
}

/// How long the zoom-level pill is up, fades included.
pub(super) const ZOOM_TOAST: Duration = Duration::from_millis(1300);
/// How long an error toast stays before it leaves on its own.
pub(super) const NOTICE_HOLD: Duration = Duration::from_secs(6);
pub(super) const NOTICE_ENTER: Duration = Duration::from_millis(240);
pub(super) const NOTICE_EXIT: Duration = Duration::from_millis(160);

/// How long reasoning deltas gather before their rows are rebuilt.
const PUBLISH_BATCH: Duration = Duration::from_millis(33);

/// What a turn's cached file chips were computed under: finished tool
/// calls in the turn, and the turn generation (bumped when a turn ends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TurnChipKey {
    finished: usize,
    turn_gen: u64,
}

/// The message as sent: a picked command first (the CLIs only expand one
/// that leads the message), then what was typed, then the other references,
/// one per line: `@path` for files and folders, a markdown link for an issue
/// (so the agent gets its number, title and address).
fn with_refs(typed: &str, refs: &[MentionItem]) -> String {
    let command = refs.iter().find_map(|item| match item {
        MentionItem::Command(c) => Some(c.insert.as_str()),
        _ => None,
    });
    let lead = match (command, typed.is_empty()) {
        (Some(command), true) => command.to_string(),
        (Some(command), false) => format!("{command} {typed}"),
        (None, _) => typed.to_string(),
    };
    let refs: Vec<String> = refs
        .iter()
        .filter_map(|item| match item {
            MentionItem::Command(_) => None,
            MentionItem::Path(path) => Some(format!("@{path}")),
            MentionItem::Issue(i) => {
                let title: String = i.title.chars().filter(|ch| !matches!(ch, '[' | ']')).collect();
                Some(format!("[#{} {title}]({})", i.iid, i.web_url))
            }
        })
        .collect();
    match (lead.is_empty(), refs.is_empty()) {
        (_, true) => lead,
        (true, false) => refs.join("\n"),
        (false, false) => format!("{lead}\n\n{}", refs.join("\n")),
    }
}

/// Rows the `@` menu shows at most: files, and issues above them.
const MENTION_ROWS: usize = 8;
const MENTION_ISSUE_ROWS: usize = 5;
const MENTION_COMMAND_ROWS: usize = 10;

/// One row of the `@` menu.
#[derive(Clone, PartialEq)]
enum MentionItem {
    Issue(gitlab::Issue),
    Path(String),
    /// A card like the rest, but it leads the message when sent.
    Command(SlashCommand),
}

impl MentionItem {
    /// Stable identity for a card: the issue's address or the path.
    fn key(&self) -> String {
        match self {
            MentionItem::Issue(i) => i.web_url.clone(),
            MentionItem::Path(p) => p.clone(),
            MentionItem::Command(c) => c.insert.clone(),
        }
    }
}

/// The run of consecutive messages around `pos` that `same` accepts:
/// one assistant turn. `pos` itself is assumed to be in it.
fn turn_span(order: &[String], pos: usize, same: impl Fn(&String) -> bool) -> (usize, usize) {
    let mut start = pos;
    while start > 0 && same(&order[start - 1]) {
        start -= 1;
    }
    let mut end = pos;
    while end + 1 < order.len() && same(&order[end + 1]) {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
mod turn_tests {
    use super::turn_span;

    #[test]
    fn a_turn_runs_between_user_messages() {
        let order: Vec<String> = ["u1", "a1", "a2", "a3", "u2", "a4"].map(String::from).to_vec();
        let assistant = |m: &String| m.starts_with('a');
        assert_eq!(turn_span(&order, 2, assistant), (1, 3));
        assert_eq!(turn_span(&order, 1, assistant), (1, 3));
        assert_eq!(turn_span(&order, 5, assistant), (5, 5));
    }

    #[test]
    fn is_step_row_identifies_work_blocks() {
        use crate::markdown::{ToolEntry, TranscriptBlock};
        assert!(super::is_step_row(&TranscriptBlock::Reasoning {
            id: "m#reasoning0".into(),
            content: "thinking".into(),
            expanded: false,
        }));
        let tool = ToolEntry {
            tool_id: "t1".into(),
            name: "bash".into(),
            summary: "ls".into(),
            state: "done".into(),
            started_ms: Some(100),
            ended_ms: Some(200),
            images: Vec::new(),
            ..Default::default()
        };
        assert!(super::is_step_row(&TranscriptBlock::ToolGroup {
            id: "m#tools0".into(),
            tools: vec![tool],
            expanded: false,
            overrides: Vec::new(),
        }));
        assert!(super::is_step_row(&TranscriptBlock::Shots {
            id: "m#tools0#shots".into(),
            paths: vec!["/path/screenshot.png".into()],
        }));
        assert!(!super::is_step_row(&TranscriptBlock::Shots {
            id: "m#p1".into(),
            paths: vec!["/path/image.png".into()],
        }));
        assert!(super::is_step_row(&TranscriptBlock::Plan {
            id: "m#plan".into(),
            entries: Vec::new(),
            live: false,
            expanded: None,
        }));
        assert!(super::is_step_row(&TranscriptBlock::Subagent {
            id: "m#sub-1".into(),
            subagent_id: "1".into(),
            name: "agent".into(),
            state: "done".into(),
            summary: "".into(),
        }));
        assert!(!super::is_step_row(&TranscriptBlock::Paragraph {
            id: "m#t0".into(),
            content: "Hello".into(),
            spans: Vec::new(),
        }));
    }

    #[test]
    fn worked_summary_collapses_to_show_and_hide_works() {
        use crate::markdown::{ToolEntry, TranscriptBlock};
        let done_tool = ToolEntry {
            tool_id: "t1".into(),
            name: "bash".into(),
            summary: "ls".into(),
            state: "done".into(),
            started_ms: Some(100),
            ended_ms: Some(200),
            images: Vec::new(),
            ..Default::default()
        };
        let rows = vec![
            TranscriptBlock::Reasoning {
                id: "m#reasoning0".into(),
                content: "thinking".into(),
                expanded: false,
            },
            TranscriptBlock::ToolGroup {
                id: "m#tools0".into(),
                tools: vec![done_tool.clone()],
                expanded: false,
                overrides: Vec::new(),
            },
            TranscriptBlock::Paragraph {
                id: "m#t0".into(),
                content: "Done!".into(),
                spans: Vec::new(),
            },
        ];

        // Collapsed state
        let summary_closed = super::worked_summary(&rows, "m#worked".into(), false).unwrap();
        let TranscriptBlock::TurnSummary { label, failed, expanded, .. } = summary_closed else { panic!() };
        assert_eq!(label, "2 steps completed · View trace");
        assert!(!failed);
        assert!(!expanded);

        // Expanded state
        let summary_open = super::worked_summary(&rows, "m#worked".into(), true).unwrap();
        let TranscriptBlock::TurnSummary { label, failed, expanded, .. } = summary_open else { panic!() };
        assert_eq!(label, "2 steps completed · Hide trace");
        assert!(!failed);
        assert!(expanded);

        // When a tool is still running: returns None
        let running_tool = ToolEntry {
            state: "running".into(),
            ..done_tool.clone()
        };
        let running_rows = vec![TranscriptBlock::ToolGroup {
            id: "m#tools0".into(),
            tools: vec![running_tool],
            expanded: false,
            overrides: Vec::new(),
        }];
        assert!(super::worked_summary(&running_rows, "m#worked".into(), false).is_none());

        // Duration formatted when >= 1s
        let timed_tool1 = ToolEntry {
            started_ms: Some(1000),
            ended_ms: Some(2000),
            ..done_tool.clone()
        };
        let timed_tool2 = ToolEntry {
            started_ms: Some(2000),
            ended_ms: Some(3400),
            ..done_tool.clone()
        };
        let timed_rows = vec![TranscriptBlock::ToolGroup {
            id: "m#tools0".into(),
            tools: vec![timed_tool1.clone(), timed_tool2],
            expanded: false,
            overrides: Vec::new(),
        }];
        let timed_summary = super::worked_summary(&timed_rows, "m#worked".into(), false).unwrap();
        let TranscriptBlock::TurnSummary { label, failed, .. } = timed_summary else { panic!() };
        assert_eq!(label, "2 steps completed in 2.4s · View trace");
        assert!(!failed);

        // > 60 seconds formats in minutes (e.g. 158.1s -> 2m 38s)
        let min_tool2 = ToolEntry {
            started_ms: Some(2000),
            ended_ms: Some(1000 + 158_100),
            ..done_tool.clone()
        };
        let min_rows = vec![TranscriptBlock::ToolGroup {
            id: "m#tools0".into(),
            tools: vec![timed_tool1.clone(), min_tool2],
            expanded: false,
            overrides: Vec::new(),
        }];
        let min_summary = super::worked_summary(&min_rows, "m#worked".into(), false).unwrap();
        let TranscriptBlock::TurnSummary { label, .. } = min_summary else { panic!() };
        assert_eq!(label, "2 steps completed in 2m 38s · View trace");

        // > 60 minutes formats in hours
        let hour_tool2 = ToolEntry {
            started_ms: Some(2000),
            ended_ms: Some(1000 + 3_665_000),
            ..done_tool.clone()
        };
        let hour_rows = vec![TranscriptBlock::ToolGroup {
            id: "m#tools0".into(),
            tools: vec![timed_tool1, hour_tool2],
            expanded: false,
            overrides: Vec::new(),
        }];
        let hour_summary = super::worked_summary(&hour_rows, "m#worked".into(), false).unwrap();
        let TranscriptBlock::TurnSummary { label, .. } = hour_summary else { panic!() };
        assert_eq!(label, "2 steps completed in 1h 1m 5s · View trace");

        // Failures formatted cleanly
        let failed_tool = ToolEntry {
            state: "error".into(),
            ..done_tool
        };
        let failed_rows = vec![TranscriptBlock::ToolGroup {
            id: "m#tools0".into(),
            tools: vec![failed_tool],
            expanded: false,
            overrides: Vec::new(),
        }];
        let failed_summary = super::worked_summary(&failed_rows, "m#worked".into(), false).unwrap();
        let TranscriptBlock::TurnSummary { label, failed, .. } = failed_summary else { panic!() };
        assert_eq!(label, "1 step failed · View trace");
        assert!(failed);
    }

    #[test]
    fn fold_turn_rows_hides_intermediate_responses_when_collapsed() {
        use crate::markdown::{ToolEntry, TranscriptBlock};
        let done_tool = ToolEntry {
            tool_id: "t1".into(),
            name: "bash".into(),
            summary: "ls".into(),
            state: "done".into(),
            started_ms: Some(100),
            ended_ms: Some(200),
            images: Vec::new(),
            ..Default::default()
        };
        let rows = vec![
            TranscriptBlock::Reasoning {
                id: "m#reasoning0".into(),
                content: "thinking".into(),
                expanded: false,
            },
            TranscriptBlock::Paragraph {
                id: "m#t0".into(),
                content: "I'll pull the original reports behind those two figures, not just the reprints.".into(),
                spans: Vec::new(),
            },
            TranscriptBlock::ToolGroup {
                id: "m#tools0".into(),
                tools: vec![done_tool.clone()],
                expanded: false,
                overrides: Vec::new(),
            },
            TranscriptBlock::Reasoning {
                id: "m#reasoning1".into(),
                content: "evaluating".into(),
                expanded: false,
            },
            TranscriptBlock::Paragraph {
                id: "m#t1".into(),
                content: "Here are the figures from the original reports.".into(),
                spans: Vec::new(),
            },
        ];

        let summary_closed = super::worked_summary(&rows, "m#worked".into(), false).unwrap();
        let folded_closed = super::fold_turn_rows(rows.clone(), summary_closed, false);
        // When collapsed: only TurnSummary ("3 steps completed · View trace") and the final response ("Here are the figures...")
        assert_eq!(folded_closed.len(), 2);
        assert!(matches!(&folded_closed[0], TranscriptBlock::TurnSummary { label, .. } if label == "3 steps completed · View trace"));
        assert!(matches!(&folded_closed[1], TranscriptBlock::Paragraph { content, .. } if content == "Here are the figures from the original reports."));

        let summary_open = super::worked_summary(&rows, "m#worked".into(), true).unwrap();
        let folded_open = super::fold_turn_rows(rows, summary_open, true);
        // When expanded: TurnSummary ("3 steps completed · Hide trace"), Thought, intermediate Paragraph, ToolGroup, Thought, final Paragraph
        assert_eq!(folded_open.len(), 6);
        assert!(matches!(&folded_open[0], TranscriptBlock::TurnSummary { label, .. } if label == "3 steps completed · Hide trace"));
        assert!(matches!(&folded_open[1], TranscriptBlock::Reasoning { .. }));
        assert!(matches!(&folded_open[2], TranscriptBlock::Paragraph { content, .. } if content == "I'll pull the original reports behind those two figures, not just the reprints."));
        assert!(matches!(&folded_open[3], TranscriptBlock::ToolGroup { .. }));
        assert!(matches!(&folded_open[4], TranscriptBlock::Reasoning { .. }));
        assert!(matches!(&folded_open[5], TranscriptBlock::Paragraph { content, .. } if content == "Here are the figures from the original reports."));
    }

    #[test]
    fn file_changes_are_pinned_at_the_end_after_the_final_response() {
        use crate::markdown::{ToolEntry, TranscriptBlock};
        use crate::file_changes::FileChanges;
        let done_tool = ToolEntry {
            tool_id: "t1".into(),
            name: "edit".into(),
            summary: "edit foo.rs".into(),
            state: "done".into(),
            started_ms: Some(100),
            ended_ms: Some(200),
            ..Default::default()
        };
        let file_card = FileChanges::from_spec(&serde_json::json!({
            "component": "files",
            "root": "/w",
            "more": 0,
            "files": [{"path": "src/foo.rs", "status": "M"}]
        })).unwrap();

        let mut rows = vec![
            TranscriptBlock::ToolGroup {
                id: "m#tools0".into(),
                tools: vec![done_tool],
                expanded: false,
                overrides: Vec::new(),
            },
            TranscriptBlock::FileChanges {
                id: "m#ui-files".into(),
                card: file_card,
                expanded: false,
            },
            TranscriptBlock::Paragraph {
                id: "m#t1".into(),
                content: "Done editing foo.rs.".into(),
                spans: Vec::new(),
            },
        ];

        let mut file_changes = Vec::new();
        rows.retain(|row| {
            if matches!(row, TranscriptBlock::FileChanges { .. }) {
                file_changes.push(row.clone());
                false
            } else {
                true
            }
        });

        let summary = super::worked_summary(&rows, "m#worked".into(), false).unwrap();
        rows = super::fold_turn_rows(rows, summary, false);
        rows.extend(file_changes);

        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], TranscriptBlock::TurnSummary { .. }));
        assert!(matches!(&rows[1], TranscriptBlock::Paragraph { content, .. } if content == "Done editing foo.rs."));
        assert!(matches!(&rows[2], TranscriptBlock::FileChanges { expanded, .. } if !expanded));
    }

    #[test]
    fn file_changes_defaults_to_collapsed() {
        use crate::file_changes::FileChanges;
        use crate::markdown::TranscriptBlock;
        let file_card = FileChanges::from_spec(&serde_json::json!({
            "component": "files",
            "root": "/w",
            "more": 0,
            "files": [{"path": "src/foo.rs", "status": "M"}]
        })).unwrap();

        let block = TranscriptBlock::FileChanges {
            id: "m#files".into(),
            card: file_card,
            expanded: false,
        };

        if let TranscriptBlock::FileChanges { expanded, .. } = block {
            assert!(!expanded, "FileChanges component must default to collapsed");
        } else {
            panic!("expected FileChanges");
        }
    }
}

#[cfg(test)]
mod refs_tests {
    use super::*;

    #[test]
    fn picked_refs_follow_the_text_one_per_line() {
        let issue = gitlab::Issue {
            iid: 33,
            title: "Feature [MFA] Login".into(),
            state: "opened".into(),
            labels: Vec::new(),
            author: String::new(),
            comments: 0,
            updated_at: String::new(),
            web_url: "https://gl/x/-/issues/33".into(),
        };
        let refs = [MentionItem::Path("src/a.json".into()), MentionItem::Issue(issue)];
        assert_eq!(with_refs("fix it", &refs), "fix it\n\n@src/a.json\n[#33 Feature MFA Login](https://gl/x/-/issues/33)");
        assert_eq!(with_refs("", &refs[..1]), "@src/a.json");
        assert_eq!(with_refs("just text", &[]), "just text");
        // A command leads, whatever the order it was picked in.
        let command = MentionItem::Command(SlashCommand {
            name: "brandkit".into(),
            insert: "/skill:brandkit".into(),
            description: String::new(),
            skill: true,
            project: true,
        });
        assert_eq!(
            with_refs("make a logo", &[refs[0].clone(), command.clone()]),
            "/skill:brandkit make a logo\n\n@src/a.json"
        );
        assert_eq!(with_refs("", &[command]), "/skill:brandkit");
    }
}

impl KeelWindow {
    fn open_lightbox(&mut self, lightbox: Lightbox, cx: &mut Context<Self>) {
        self.lightbox = Some(lightbox);
        self.lightbox_fx.set(true, std::time::Instant::now(), self.reduced());
        cx.notify();
    }

    /// Start the overlay's exit; false when none is open.
    fn close_lightbox(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.lightbox_fx.shown() {
            return false;
        }
        self.lightbox_fx.set(false, std::time::Instant::now(), self.reduced());
        cx.notify();
        true
    }
}

/// What the full-window overlay shows.
#[derive(Clone)]
enum Lightbox {
    Image(std::path::PathBuf),
    /// A `mermaid` fence, to pan and zoom. `row_id` keys its "Copied" states.
    Diagram { row_id: String, source: String, view: crate::mermaid_viewer::SharedViewport },
    /// A scrollable full output viewer for truncated tool/code blocks.
    Output { title: Option<String>, content: String, error: bool },
}

/// Rows that are the turn's work (folded into "Show works").
fn is_step_row(row: &TranscriptBlock) -> bool {
    match row {
        TranscriptBlock::ToolGroup { tools, .. } => !(tools.len() == 1 && tools[0].is_receipt()),
        TranscriptBlock::Reasoning { .. } | TranscriptBlock::Plan { .. } | TranscriptBlock::Subagent { .. } => true,
        TranscriptBlock::Shots { id, .. } => id.contains("#tools"),
        _ => false,
    }
}

/// "Show works" / "Hide works" over the turn's steps; None when the turn
/// did no visible work or is still working.
fn worked_summary(rows: &[TranscriptBlock], id: String, expanded: bool) -> Option<TranscriptBlock> {
    let tools: Vec<_> = rows
        .iter()
        .filter(|row| is_step_row(row))
        .flat_map(|row| match row {
            TranscriptBlock::ToolGroup { tools, .. } => tools.as_slice(),
            _ => &[],
        })
        .collect();
    let running_subagent = rows.iter().any(|r| matches!(r, TranscriptBlock::Subagent { state, .. } if state == "running"));
    let live_plan = rows.iter().any(|r| matches!(r, TranscriptBlock::Plan { live: true, .. }));
    if !rows.iter().any(is_step_row)
        || tools.iter().any(|t| t.state == "running" || t.state == "pending")
        || running_subagent
        || live_plan
    {
        return None;
    }
    let failed = tools.iter().filter(|t| t.state == "failed" || t.state == "error").count() as u64
        + rows
            .iter()
            .filter(|r| matches!(r, TranscriptBlock::Subagent { state, .. } if matches!(state.as_str(), "error" | "failed" | "errored")))
            .count() as u64;

    let other_steps = rows
        .iter()
        .filter(|r| matches!(r, TranscriptBlock::Reasoning { .. } | TranscriptBlock::Plan { .. } | TranscriptBlock::Subagent { .. }))
        .count();
    let count = (tools.len() + other_steps).max(1);

    let started = tools.iter().filter_map(|t| t.started_ms).min();
    let ended = tools.iter().filter_map(|t| t.ended_ms).max();
    let duration = match (started, ended) {
        (Some(start), Some(end)) if end > start && (end - start) >= 1000 => {
            let ms = (end - start) as u64;
            let formatted = if ms < 60_000 {
                format!("{:.1}s", ms as f64 / 1000.0)
            } else {
                crate::trace::format_duration(ms)
            };
            format!(" in {formatted}")
        }
        _ => String::new(),
    };

    let action = if expanded { "Hide trace" } else { "View trace" };

    let label = if failed > 0 {
        if failed == 1 && count == 1 {
            format!("1 step failed{duration} · {action}")
        } else if failed == count as u64 {
            format!("{count} steps failed{duration} · {action}")
        } else {
            format!("{failed} of {count} steps failed{duration} · {action}")
        }
    } else {
        format!("{count} step{} completed{duration} · {action}", if count == 1 { "" } else { "s" })
    };

    Some(TranscriptBlock::TurnSummary { id, label, failed: failed > 0, expanded })
}

/// Folds a finished turn's rows: hides all steps and intermediate responses
/// occurring before or during the work when collapsed (`!open`).
fn fold_turn_rows(mut rows: Vec<TranscriptBlock>, summary: TranscriptBlock, open: bool) -> Vec<TranscriptBlock> {
    if let Some(last) = rows.iter().rposition(is_step_row) {
        if !open {
            let final_rows = rows.split_off(last + 1);
            rows.clear();
            rows.extend(final_rows);
        }
        rows.insert(0, summary);
    }
    rows
}

/// The model section's skeleton-to-rows crossfade and height glide.
pub(super) const MODEL_FADE_MS: u64 = 220;
