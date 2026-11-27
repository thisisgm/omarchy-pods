use std::collections::HashMap;

use pods_core::command::{AdaptiveLevel, Command, NoiseTarget};
use pods_core::state::{EarDetectionBehavior, State};
use pods_proto::noise::NoiseMode;
use zbus::object_server::SignalEmitter;
use zbus::{OwnedVariant, Result, Variant, fdo, interface};

use super::Hub;
use super::item::{battery_summary, plain_label};

pub(super) const MENU_PATH: &str = "/MenuBar";
const MENU_VERSION: u32 = 3;
const ROOT: i32 = 0;
const ADAPTIVE_MENU: i32 = 8;
const MODES: [(i32, &str, NoiseMode); 4] = [
    (4, "Off", NoiseMode::Off),
    (5, "Noise Cancellation", NoiseMode::Anc),
    (6, "Transparency", NoiseMode::Transparency),
    (7, "Adaptive", NoiseMode::Adaptive),
];
const PRESETS: [(i32, u8); 5] = [(13, 0), (14, 25), (15, 50), (16, 75), (17, 100)];

type Properties = HashMap<String, OwnedVariant>;
type Layout = (i32, Properties, Vec<OwnedVariant>);
type ItemProperties = Vec<(i32, Properties)>;
type RemovedProperties = Vec<(i32, Vec<String>)>;
type Event = (i32, String, OwnedVariant, u32);

#[derive(Debug, PartialEq)]
struct Row {
    id: i32,
    parent: i32,
    properties: HashMap<String, Variant<'static>>,
    command: Option<Command>,
}

impl Row {
    fn new<const N: usize>(id: i32, parent: i32, fields: [(&str, Variant<'static>); N]) -> Self {
        let properties = fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect();
        Self {
            id,
            parent,
            properties,
            command: None,
        }
    }

    fn text(id: i32, label: String) -> Self {
        Self::new(
            id,
            ROOT,
            [("label", label.into()), ("enabled", false.into())],
        )
    }

    fn separator(id: i32) -> Self {
        Self::new(id, ROOT, [("type", "separator".into())])
    }

    fn toggle(
        id: i32,
        parent: i32,
        label: String,
        kind: &str,
        selected: bool,
        command: Command,
    ) -> Self {
        let mut row = Self::new(
            id,
            parent,
            [
                ("label", label.into()),
                ("toggle-type", kind.to_owned().into()),
                ("toggle-state", i32::from(selected).into()),
            ],
        );
        row.command = Some(command);
        row
    }

    fn properties(&self, names: &[String]) -> Result<Properties> {
        self.properties
            .iter()
            .filter(|(name, _)| names.is_empty() || names.contains(name))
            .map(|(name, value)| Ok((name.clone(), value.try_to_owned()?)))
            .collect()
    }
}

fn rows(state: &State) -> Vec<Row> {
    let mut rows = vec![
        Row::new(ROOT, ROOT, [("children-display", "submenu".into())]),
        Row::text(
            1,
            if state.connected {
                // DBusMenu uses underscores for keyboard mnemonics.
                plain_label(&state.device_name).replace('_', "__")
            } else {
                "AirPods".to_owned()
            },
        ),
    ];
    let battery = battery_summary(state);
    if !battery.is_empty() {
        rows.push(Row::text(2, battery));
    }
    if !state.connected {
        return rows;
    }
    let caps = state.capabilities();
    if caps.noise_control {
        rows.push(Row::separator(3));
        for (id, label, mode) in MODES {
            let available = match mode {
                NoiseMode::Off => caps.noise_off,
                NoiseMode::Anc | NoiseMode::Transparency => true,
                NoiseMode::Adaptive => caps.adaptive,
            };
            if available {
                rows.push(Row::toggle(
                    id,
                    ROOT,
                    label.to_owned(),
                    "radio",
                    state.noise_mode == mode,
                    Command::SetNoise(NoiseTarget::Mode(mode)),
                ));
            }
        }
    }
    if caps.adaptive && state.noise_mode == NoiseMode::Adaptive {
        let level = state.adaptive_noise_level.percent();
        rows.push(Row::new(
            ADAPTIVE_MENU,
            ROOT,
            [
                ("label", format!("Adaptive Noise ({level}%)").into()),
                ("children-display", "submenu".into()),
            ],
        ));
        for (id, percent) in PRESETS {
            if let Ok(preset) = AdaptiveLevel::new(percent) {
                rows.push(Row::toggle(
                    id,
                    ADAPTIVE_MENU,
                    format!("{percent}%"),
                    "radio",
                    level == percent,
                    Command::SetAdaptiveLevel(preset),
                ));
            }
        }
    }
    add_switches(&mut rows, state);
    rows
}

fn add_switches(rows: &mut Vec<Row>, state: &State) {
    let caps = state.capabilities();
    if caps.conversational_awareness || caps.ear_detection || caps.one_bud_anc {
        rows.push(Row::separator(9));
    }
    let ear = state.ear_detection_behavior != EarDetectionBehavior::Disabled;
    let next_ear = if ear {
        EarDetectionBehavior::Disabled
    } else {
        EarDetectionBehavior::default()
    };
    for (id, label, available, selected, command) in [
        (
            10,
            "Conversation Awareness",
            caps.conversational_awareness,
            state.conversational_awareness,
            Command::SetConversationalAwareness(!state.conversational_awareness),
        ),
        (
            11,
            "Ear Detection",
            caps.ear_detection,
            ear,
            Command::SetEarDetection(next_ear),
        ),
        (
            12,
            "One-Bud ANC",
            caps.one_bud_anc,
            state.one_bud_anc_mode,
            Command::SetOneBudAnc(!state.one_bud_anc_mode),
        ),
    ] {
        if available {
            rows.push(Row::toggle(
                id,
                ROOT,
                label.to_owned(),
                "checkmark",
                selected,
                command,
            ));
        }
    }
}

#[derive(Debug)]
pub(super) struct Menu {
    hub: Hub,
    revision: u32,
    rows: Vec<Row>,
}

impl Menu {
    pub(super) fn new(hub: Hub, state: &State) -> Self {
        Self {
            hub,
            revision: 1,
            rows: rows(state),
        }
    }

    pub(super) fn update(&mut self, state: &State) -> Option<u32> {
        let rows = rows(state);
        if self.rows == rows {
            return None;
        }
        self.rows = rows;
        self.revision = self.revision.wrapping_add(1);
        Some(self.revision)
    }

    fn row(&self, id: i32) -> Option<&Row> {
        self.rows.iter().find(|row| row.id == id)
    }

    fn layout(&self, row: &Row, depth: i32, names: &[String]) -> Result<Layout> {
        let mut children = Vec::new();
        if depth != 0 {
            for child in self
                .rows
                .iter()
                .filter(|child| child.parent == row.id && child.id != ROOT)
            {
                let layout = self.layout(child, depth.saturating_sub(1), names)?;
                children.push(Variant::from(layout).try_to_owned()?);
            }
        }
        Ok((row.id, row.properties(names)?, children))
    }

    fn dispatch(&self, id: i32, event: &str) -> bool {
        let Some(row) = self.row(id) else {
            return false;
        };
        if event == "clicked"
            && let Some(command) = row.command
        {
            // Hub queues synchronously with try_send. The state subscription publishes the result.
            drop(self.hub.submit(command));
        }
        true
    }
}

#[interface(name = "com.canonical.dbusmenu")]
#[expect(
    clippy::unused_self,
    reason = "D-Bus exposes constants and no-ops as instance members"
)]
impl Menu {
    #[zbus(property(emits_changed_signal = "const"))]
    fn version(&self) -> u32 {
        MENU_VERSION
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn text_direction(&self) -> &'static str {
        "ltr"
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn status(&self) -> &'static str {
        "normal"
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "D-Bus arrays deserialize into owned vectors"
    )]
    #[zbus(out_args("revision", "layout"))]
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> fdo::Result<(u32, Layout)> {
        let row = self
            .row(parent_id)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown menu item {parent_id}")))?;
        self.layout(row, recursion_depth, &property_names)
            .map(|layout| (self.revision, layout))
            .map_err(|error| fdo::Error::Failed(error.to_string()))
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "D-Bus arrays deserialize into owned vectors"
    )]
    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> fdo::Result<ItemProperties> {
        let ids = if ids.is_empty() {
            self.rows.iter().map(|row| row.id).collect()
        } else {
            ids
        };
        ids.into_iter()
            .filter_map(|id| self.row(id))
            .map(|row| {
                row.properties(&property_names)
                    .map(|properties| (row.id, properties))
                    .map_err(|error| fdo::Error::Failed(error.to_string()))
            })
            .collect()
    }

    fn get_property(&self, id: i32, name: &str) -> fdo::Result<OwnedVariant> {
        let row = self
            .row(id)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown menu item {id}")))?;
        let default = match name {
            "type" => Some(Variant::from("standard")),
            "label" => Some(Variant::from("")),
            "enabled" | "visible" => Some(Variant::from(true)),
            _ => None,
        };
        row.properties
            .get(name)
            .or(default.as_ref())
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown menu property {name}")))?
            .try_to_owned()
            .map_err(|error| fdo::Error::Failed(error.to_string()))
    }

    fn event(&self, id: i32, event_id: &str, data: Variant<'_>, timestamp: u32) {
        drop((data, timestamp));
        self.dispatch(id, event_id);
    }

    fn event_group(&self, events: Vec<Event>) -> Vec<i32> {
        events
            .into_iter()
            .filter_map(|(id, event, _, _)| (!self.dispatch(id, &event)).then_some(id))
            .collect()
    }

    fn about_to_show(&self, id: i32) -> bool {
        let _ = id;
        false
    }

    #[zbus(out_args("updates_needed", "id_errors"))]
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (
            Vec::new(),
            ids.into_iter()
                .filter(|&id| self.row(id).is_none())
                .collect(),
        )
    }

    #[zbus(signal)]
    pub(super) async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> Result<()>;

    #[zbus(signal)]
    async fn items_properties_updated(
        emitter: &SignalEmitter<'_>,
        updated: ItemProperties,
        removed: RemovedProperties,
    ) -> Result<()>;
}
