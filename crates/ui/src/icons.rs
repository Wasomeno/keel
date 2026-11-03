//! Lucide icons (ISC), embedded. `Assets` serves them to gpui's `svg()`.

use std::borrow::Cow;

use gpui::{
    App, AssetSource, Bounds, Element, ElementId, GlobalElementId, Hitbox, InspectorElementId,
    IntoElement, LayoutId, Pixels, SharedString, StyleRefinement, Styled, Svg, Transformation,
    Window, svg,
};

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        /// An icon in `crates/ui/assets/icons`.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon { $($variant),* }

        impl Icon {
            pub const ALL: &[Icon] = &[$(Icon::$variant),*];

            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $file, ".svg")),* }
            }

            fn bytes(path: &str) -> Option<&'static [u8]> {
                match path {
                    $(concat!("icons/", $file, ".svg") => Some(include_bytes!(concat!("../assets/icons/", $file, ".svg"))),)*
                    _ => None,
                }
            }
        }
    };
}

icons! {
    Plus => "plus",
    Minus => "minus",
    Ellipsis => "ellipsis",
    ChevronDown => "chevron-down",
    Close => "close",
    Check => "check",
    Folder => "folder",
    Archive => "inbox",
    Send => "arrow-up",
    Stop => "square",
    Alert => "triangle-alert",
    Tool => "square-terminal",
    SidebarClose => "panel-left-close",
    SidebarOpen => "panel-left-open",
    Spinner => "loader-circle",
    Copy => "copy",
    Sun => "sun",
    Moon => "moon",
    Settings => "settings",
    Branch => "git-branch",
    Paperclip => "paperclip",
    Image => "image",
    Back => "arrow-left",
    Forward => "arrow-right",
    Monitor => "monitor",
    Trash => "trash-2",
    File => "file",
    FileDiff => "file-diff",
    GitLab => "git-pull-request",
    Issue => "circle-dot",
    RepoNode => "repo-node",
    Comment => "message-square",
    FileCode => "file-code",
    FileJson => "file-json",
    FileText => "file-text",
    Brain => "brain",
    Puzzle => "puzzle",
    // The effort chip: one sparkle per level lit.
    Sparkle => "sparkle",
    SparkleFilled => "sparkle-filled",
    Globe => "globe",
    Camera => "camera",
    PointerClick => "mouse-pointer-click",
    Keyboard => "keyboard",
    Mouse => "mouse",
    Reload => "rotate-cw",
    External => "external-link",
    Expand => "maximize-2",
    Shrink => "minimize-2",
    Power => "power",
    Timer => "timer",
    Pause => "pause",
    Reset => "rotate-ccw",
    History => "history",
    Star => "star",
    StarFilled => "star-filled",
    CloudOff => "cloud-off",
    Eye => "eye",
    EyeOff => "eye-off",
    Clipboard => "clipboard",
    Search => "search",
    FilePen => "file-pen",
    ListChecks => "list-checks",
    Agent => "bot",
    Circle => "circle",
    Terminal => "terminal",
    MoveRight => "move-right",
    List => "list",
    ArrowDown => "arrow-down",
    ArrowUpDown => "arrow-up-down",
    TabPlus => "tab-plus",
    AppWindow => "app-window",
    Code => "code",
    ScrollText => "scroll-text",
    Network => "network",
    ListTree => "list-tree",
    FolderTree => "folder-tree",
    Dialog => "message-square-warning",
    AgentOff => "bot-off",
    Play => "play",
    Volume2 => "volume-2",
    // Provider logos: Lobe Icons (MIT); Command Code from commandcode.ai.
    LogoClaude => "logo-claude",
    LogoOpenAi => "logo-openai",
    LogoGrok => "logo-grok",
    LogoKeel => "logo-keel",
    LogoCommandCode => "logo-command-code",
    LogoCopilot => "logo-copilot",
}

impl Icon {
    /// Monochrome icon in the surrounding text color (hover included), or
    /// its own `text_color` when set.
    pub fn el(self, size: Pixels) -> IconSvg {
        IconSvg(svg().path(self.path()).size(size).flex_none())
    }
}

impl Icon {
    /// The logo for a Pi provider id (Settings → Accounts); a bot for any other.
    pub fn provider(id: &str) -> Icon {
        match id {
            "anthropic" => Icon::LogoClaude,
            "openai" | "openai-codex" => Icon::LogoOpenAi,
            "github-copilot" => Icon::LogoCopilot,
            "commandcode" => Icon::LogoCommandCode,
            "xai" => Icon::LogoGrok,
            _ => Icon::Agent,
        }
    }

    /// The logo for a harness id (`keel`); a terminal for any other.
    pub fn harness(id: &str) -> Icon {
        match id {
            "keel" => Icon::LogoKeel,
            _ => Icon::Tool,
        }
    }
}

impl Icon {
    /// The icon for a referenced path: a folder (trailing `/`), or a file by
    /// its kind: code, data, prose, image, anything else.
    pub fn for_path(path: &str) -> Icon {
        if path.ends_with('/') {
            return Icon::Folder;
        }
        let ext = path
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "py" | "go" | "rb" | "java" | "kt"
            | "swift" | "c" | "h" | "cpp" | "cs" | "php" | "vue" | "svelte" | "html" | "css"
            | "scss" | "sh" | "sql" => Icon::FileCode,
            "json" | "yaml" | "yml" | "toml" | "xml" | "csv" | "lock" | "env" => Icon::FileJson,
            "md" | "mdx" | "txt" | "rst" | "pdf" => Icon::FileText,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => Icon::Image,
            _ => Icon::File,
        }
    }
}

impl IntoElement for Icon {
    type Element = IconSvg;
    fn into_element(self) -> IconSvg {
        self.el(gpui::px(14.))
    }
}

/// An `Svg` that inherits the text color. gpui's `Svg` paints only with a
/// color set on itself and ignores its parents', which left every icon
/// invisible; this fills that color in from the text style at paint time.
pub struct IconSvg(Svg);

impl IconSvg {
    pub fn with_transformation(self, transformation: Transformation) -> Self {
        Self(self.0.with_transformation(transformation))
    }
}

impl Styled for IconSvg {
    fn style(&mut self) -> &mut StyleRefinement {
        self.0.style()
    }
}

impl IntoElement for IconSvg {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for IconSvg {
    type RequestLayoutState = <Svg as Element>::RequestLayoutState;
    type PrepaintState = <Svg as Element>::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        self.0.id()
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.0.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.0.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        self.0
            .prepaint(id, inspector_id, bounds, layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let style = self.0.style();
        if style.text.color.is_none() {
            style.text.color = Some(window.text_style().color);
        }
        self.0
            .paint(id, inspector_id, bounds, layout, hitbox, window, cx)
    }
}

/// The new-chat hero background.
pub const HERO_IMAGE: &str = "hero.jpg";
/// Its bytes, embedded.
pub const HERO_BYTES: &[u8] = include_bytes!("../assets/hero.jpg");

/// Asset source for the window: the embedded icons and images.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path == HERO_IMAGE {
            return Ok(Some(Cow::Borrowed(HERO_BYTES)));
        }
        Ok(Icon::bytes(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .iter()
            .map(|icon| icon.path())
            .filter(|p| p.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads_as_svg() {
        for icon in Icon::ALL {
            let bytes = Assets
                .load(icon.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{icon:?} missing"));
            assert!(bytes.starts_with(b"<svg"), "{icon:?} is not an svg");
        }
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
    }
}
