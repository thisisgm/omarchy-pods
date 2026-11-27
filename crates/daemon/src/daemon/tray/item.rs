use pods_core::state::State;
use zbus::ObjectPath;
use zbus::object_server::SignalEmitter;
use zbus::{Result, interface};

use super::menu::MENU_PATH;

pub(super) const ITEM_PATH: &str = "/StatusNotifierItem";
const PIXMAP_SIZE: i32 = 22;
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Icon {
    Buds,
    Pro,
    Max,
}

impl Icon {
    fn name(self) -> &'static str {
        match self {
            Self::Buds => "pods-buds-symbolic",
            Self::Pro => "pods-pro-symbolic",
            Self::Max => "pods-max-symbolic",
        }
    }

    fn pixmaps(self) -> Pixmaps {
        // Rasterized from packaging/icons at 22x22, in network-order ARGB32.
        let bytes = match self {
            Self::Buds => include_bytes!("buds.argb"),
            Self::Pro => include_bytes!("pro.argb"),
            Self::Max => include_bytes!("max.argb"),
        };
        vec![(PIXMAP_SIZE, PIXMAP_SIZE, bytes.to_vec())]
    }
}

#[derive(Debug, Clone)]
pub(super) struct Presentation {
    status: &'static str,
    icon: Icon,
    title: String,
    battery: String,
}

impl From<&State> for Presentation {
    fn from(state: &State) -> Self {
        let battery = battery_summary(state);
        let capabilities = state.capabilities();
        let icon = if capabilities.is_headset {
            Icon::Max
        } else if capabilities.is_pro_series {
            Icon::Pro
        } else {
            Icon::Buds
        };
        Self {
            status: if state.connected || !battery.is_empty() {
                "Active"
            } else {
                "Passive"
            },
            icon,
            title: escape_markup(&plain_label(&state.device_name)),
            battery,
        }
    }
}

/// Strips control characters from a peer-supplied name and falls back to `"AirPods"`.
pub(super) fn plain_label(name: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());
    let mut pending_space = false;
    for ch in name.chars() {
        if ch.is_control() {
            pending_space = !cleaned.is_empty();
        } else {
            if pending_space && !ch.is_whitespace() && !cleaned.ends_with(char::is_whitespace) {
                cleaned.push(' ');
            }
            pending_space = false;
            cleaned.push(ch);
        }
    }
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "AirPods".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Escapes Pango/Qt rich-text delimiters for SNI tooltips.
fn escape_markup(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

impl Presentation {
    pub(super) async fn announce(
        &self,
        emitter: &SignalEmitter<'_>,
        previous: &Self,
    ) -> Result<()> {
        if self.status != previous.status {
            Item::new_status(emitter, self.status).await?;
        }
        if self.icon != previous.icon {
            Item::new_icon(emitter).await?;
        }
        if self.icon != previous.icon
            || self.title != previous.title
            || self.battery != previous.battery
        {
            Item::new_tool_tip(emitter).await?;
        }
        Ok(())
    }
}

pub(super) fn battery_summary(state: &State) -> String {
    let batteries = &state.batteries;
    [
        ("L", batteries.left),
        ("R", batteries.right),
        ("Case", batteries.case),
        ("Headset", batteries.headset),
    ]
    .into_iter()
    .filter_map(|(label, reading)| {
        reading.map(|charge| {
            let charging = if charge.charging { "+" } else { "" };
            format!("{label} {}%{charging}", charge.level)
        })
    })
    .collect::<Vec<_>>()
    .join(" · ")
}

#[derive(Debug)]
pub(super) struct Item(Presentation);

impl Item {
    pub(super) fn new(state: &State) -> Self {
        Self(Presentation::from(state))
    }

    pub(super) fn replace(&mut self, next: Presentation) -> Presentation {
        std::mem::replace(&mut self.0, next)
    }
}

#[interface(name = "org.kde.StatusNotifierItem")]
#[expect(
    clippy::unused_self,
    reason = "D-Bus exposes constants and no-ops as instance members"
)]
impl Item {
    #[zbus(property(emits_changed_signal = "const"))]
    fn category(&self) -> &'static str {
        "Hardware"
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn id(&self) -> &'static str {
        "superwhisper-pods"
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn title(&self) -> &'static str {
        "Pods"
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn status(&self) -> &'static str {
        self.0.status
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn item_is_menu(&self) -> bool {
        true
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn menu(&self) -> Result<ObjectPath<'static>> {
        ObjectPath::try_from(MENU_PATH)
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn icon_name(&self) -> &'static str {
        self.0.icon.name()
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn icon_pixmap(&self) -> Pixmaps {
        self.0.icon.pixmaps()
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn tool_tip(&self) -> (&str, Pixmaps, &str, &str) {
        (
            self.0.icon.name(),
            Vec::new(),
            &self.0.title,
            &self.0.battery,
        )
    }

    fn activate(&self, x: i32, y: i32) {
        let _ = (x, y);
    }
    fn secondary_activate(&self, x: i32, y: i32) {
        let _ = (x, y);
    }
    fn context_menu(&self, x: i32, y: i32) {
        let _ = (x, y);
    }
    fn scroll(&self, delta: i32, orientation: &str) {
        let _ = (delta, orientation);
    }

    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> Result<()>;

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> Result<()>;
}
