//! Exercises the real daemon's tray and menu on a private session bus.

#[cfg(test)]
#[expect(dead_code, reason = "each suite uses only part of the shared fixture")]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::future::poll_fn;
    use std::pin::Pin;
    use std::time::Duration;

    use pods_fake::Profile;
    use pods_samples::{ADAPTIVE_LEVEL, CONVERSATION_AWARENESS, NOISE, setting};
    use tokio::runtime::Builder as Runtime;
    use tokio::sync::mpsc;
    use zbus::connection::Builder;
    use zbus::export::futures_core::Stream;
    use zbus::export::serde::Serialize;
    use zbus::{
        Connection, DynamicType, Message, MessageStream, ObjectPath, OwnedVariant, Variant,
        interface,
    };

    use super::support::bus::{BUS_NAME, INTERFACE, OBJECT_PATH, owned_connection};
    use super::support::players::PROPERTIES;
    use super::support::rig::{ADAPTIVE, Rig};
    use super::support::{HANG_LIMIT, wait_for};

    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    const WATCHER_PATH: &str = "/StatusNotifierWatcher";
    const ITEM: &str = "org.kde.StatusNotifierItem";
    const ITEM_PATH: &str = "/StatusNotifierItem";
    const MENU: &str = "com.canonical.dbusmenu";
    const MENU_PATH: &str = "/MenuBar";
    const QUIET_WINDOW: Duration = Duration::from_millis(100);
    const POLL: Duration = Duration::from_millis(10);

    type Properties = HashMap<String, OwnedVariant>;
    type Layout = (i32, Properties, Vec<OwnedVariant>);
    type Pixmaps = Vec<(i32, i32, Vec<u8>)>;
    type ToolTip = (String, Pixmaps, String, String);

    #[derive(Debug)]
    struct Watcher(mpsc::UnboundedSender<String>);

    #[interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, path: &str) {
            self.0.send(path.to_owned()).unwrap();
        }
    }

    async fn watcher(address: &str) -> (Connection, mpsc::UnboundedReceiver<String>) {
        let (sent, received) = mpsc::unbounded_channel();
        let connection = Builder::address(address)
            .serve_at(WATCHER_PATH, Watcher(sent))
            .name(WATCHER)
            .build()
            .await
            .unwrap();
        (connection, received)
    }

    async fn registered(requests: &mut mpsc::UnboundedReceiver<String>) {
        let requested = tokio::time::timeout(HANG_LIMIT, requests.recv()).await;
        assert_eq!(
            requested
                .expect("watcher registration timed out")
                .as_deref(),
            Some(ITEM_PATH)
        );
    }

    async fn call(
        bus: &Connection,
        path: &str,
        interface: &str,
        member: &str,
        body: &(impl Serialize + DynamicType),
    ) -> Message {
        bus.call_method(Some(BUS_NAME), path, Some(interface), member, body)
            .await
            .unwrap()
    }

    async fn property(bus: &Connection, path: &str, interface: &str, name: &str) -> OwnedVariant {
        call(bus, path, PROPERTIES, "Get", &(interface, name))
            .await
            .body()
            .deserialize()
            .unwrap()
    }

    async fn layout(bus: &Connection) -> (u32, Layout) {
        call(
            bus,
            MENU_PATH,
            MENU,
            "GetLayout",
            &(0_i32, -1_i32, Vec::<String>::new()),
        )
        .await
        .body()
        .deserialize()
        .unwrap()
    }

    fn children(children: Vec<OwnedVariant>) -> Vec<Layout> {
        children
            .into_iter()
            .map(|child| Layout::try_from(child).unwrap())
            .collect()
    }

    fn label(properties: &Properties) -> &str {
        properties
            .get("label")
            .map_or("", |item| <&str>::try_from(&**item).unwrap())
    }

    fn row_at(layout: &[Layout], text: &str) -> i32 {
        layout
            .iter()
            .find(|(_, props, _)| label(props) == text)
            .unwrap()
            .0
    }

    async fn click(bus: &Connection, id: i32) {
        call(
            bus,
            MENU_PATH,
            MENU,
            "Event",
            &(id, "clicked", Variant::from(0_i32), 0_u32),
        )
        .await;
    }

    async fn signal(bus: &Connection, path: &str, interface: &str, member: &str) -> MessageStream {
        let rule = format!(
            "type='signal',sender='{BUS_NAME}',interface='{interface}',member='{member}',path='{path}'"
        );
        MessageStream::for_match_rule(rule.as_str(), bus, None)
            .await
            .unwrap()
    }

    async fn next_signal(stream: &mut MessageStream) -> Message {
        tokio::time::timeout(
            HANG_LIMIT,
            poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)),
        )
        .await
        .expect("tray signal timed out")
        .unwrap()
        .unwrap()
    }

    async fn menu_with_battery(bus: &Connection, text: &str) -> (u32, Vec<Layout>) {
        tokio::time::timeout(HANG_LIMIT, async {
            loop {
                let (revision, (_, _, menu)) = layout(bus).await;
                let rows = children(menu);
                if rows.iter().any(|(_, props, _)| label(props) == text) {
                    return (revision, rows);
                }
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .expect("menu never displayed the batteries")
    }

    async fn assert_pro_item(bus: &Connection, name: &str, summary: &str) {
        let icon = property(bus, ITEM_PATH, ITEM, "IconName").await;
        assert_eq!(String::try_from(icon).unwrap(), "pods-pro-symbolic");
        let pixmaps = property(bus, ITEM_PATH, ITEM, "IconPixmap").await;
        let pixmaps = Pixmaps::try_from(pixmaps).unwrap();
        assert_eq!(pixmaps.len(), 1);
        let (width, height, data) = &pixmaps[0];
        assert_eq!((*width, *height, data.len()), (22, 22, 22 * 22 * 4));
        assert!(data.as_chunks::<4>().0.iter().any(|argb| argb[0] == 0));
        assert!(
            data.as_chunks::<4>()
                .0
                .iter()
                .any(|argb| argb[0] == u8::MAX)
        );
        let tooltip = property(bus, ITEM_PATH, ITEM, "ToolTip").await;
        assert_eq!(
            ToolTip::try_from(tooltip).unwrap(),
            (
                "pods-pro-symbolic".to_owned(),
                vec![],
                name.to_owned(),
                summary.to_owned()
            )
        );
        let menu = property(bus, ITEM_PATH, ITEM, "Menu").await;
        assert_eq!(ObjectPath::try_from(menu).unwrap().as_str(), MENU_PATH);
    }

    async fn assert_menu_actions(
        rig: &Rig,
        device: &super::support::rig::Device,
        bus: &Connection,
        menu: &[Layout],
    ) {
        assert!(
            !menu
                .iter()
                .any(|(_, props, _)| label(props).starts_with("Adaptive Noise")),
            "Adaptive Noise submenu must stay hidden outside Adaptive mode"
        );
        click(bus, row_at(menu, "Adaptive")).await;
        device
            .pods
            .wait_heard(&setting(NOISE, ADAPTIVE.wire), 1, HANG_LIMIT)
            .unwrap();
        rig.wait_status("noise_mode", |state| {
            state["noise_mode"] == serde_json::json!(ADAPTIVE.status)
        });
        let adaptive_menu = wait_for_adaptive_menu(bus).await;
        let adaptive = adaptive_menu
            .iter()
            .find(|(_, props, _)| label(props) == "Adaptive Noise (50%)")
            .unwrap();
        let presets = children(
            adaptive
                .2
                .iter()
                .map(|value| value.try_clone().unwrap())
                .collect(),
        );
        let labels: Vec<_> = presets.iter().map(|(_, props, _)| label(props)).collect();
        assert_eq!(labels, ["0%", "25%", "50%", "75%", "100%"]);
        for (items, text, field, value, identifier, wire) in [
            (
                &presets[..],
                "75%",
                "adaptive_noise_level",
                serde_json::json!(75),
                ADAPTIVE_LEVEL,
                75,
            ),
            (
                &adaptive_menu[..],
                "Conversation Awareness",
                "conversational_awareness",
                serde_json::json!(true),
                CONVERSATION_AWARENESS,
                1,
            ),
        ] {
            click(bus, row_at(items, text)).await;
            device
                .pods
                .wait_heard(&setting(identifier, wire), 1, HANG_LIMIT)
                .unwrap();
            rig.wait_status(field, |state| state[field] == value);
            let json = property(bus, OBJECT_PATH, INTERFACE, "StatusJson").await;
            // Sample input: {"noise_mode":3,"adaptive_noise_level":75,"conversational_awareness":true}.
            let actual: serde_json::Value =
                serde_json::from_str(&String::try_from(json).unwrap()).unwrap();
            assert_eq!(actual[field], value);
            wait_for(field, || {
                let file: serde_json::Value =
                    serde_json::from_slice(&fs::read(rig.session().state_file()).ok()?).ok()?;
                (file[field] == value).then_some(())
            });
        }
        click(bus, i32::MAX).await;
    }

    async fn wait_for_adaptive_menu(bus: &Connection) -> Vec<Layout> {
        tokio::time::timeout(HANG_LIMIT, async {
            loop {
                let (_, (_, _, items)) = layout(bus).await;
                let entries = children(items);
                if entries
                    .iter()
                    .any(|(_, props, _)| label(props) == "Adaptive Noise (50%)")
                {
                    return entries;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Adaptive Noise submenu did not appear after switching to Adaptive mode")
    }

    #[test]
    fn watcher_restart_state_and_menu_actions() {
        let runtime = Runtime::new_current_thread().enable_all().build().unwrap();
        let mut rig = Rig::new();
        let profile = Profile {
            name: "AirPods Pro".to_owned(),
            model_number: "A3047".to_owned(),
            ..Profile::default()
        };
        let device = rig.add_pods(profile);
        let address = rig.session_bus().to_owned();
        let (first, mut requests) = runtime.block_on(watcher(&address));
        rig.start();
        let bus = runtime.block_on(owned_connection(&address, rig.daemon()));
        let (passive, mut status, mut updates, _second) = runtime.block_on(async {
            registered(&mut requests).await;
            let status = property(&bus, ITEM_PATH, ITEM, "Status").await;
            assert_eq!(String::try_from(status).unwrap(), "Passive");
            let (passive, (_, _, menu)) = layout(&bus).await;
            assert_eq!(children(menu).len(), 1);

            first.close().await.unwrap();
            let (second, mut requests) = watcher(&address).await;
            registered(&mut requests).await;
            let status = signal(&bus, ITEM_PATH, ITEM, "NewStatus").await;
            let updates = signal(&bus, MENU_PATH, MENU, "LayoutUpdated").await;
            (passive, status, updates, second)
        });
        rig.link_up(&device);
        rig.wait_filled();
        runtime.block_on(async {
            let active: String = next_signal(&mut status).await.body().deserialize().unwrap();
            assert_eq!(active, "Active");
            let (revision, parent): (u32, i32) = next_signal(&mut updates)
                .await
                .body()
                .deserialize()
                .unwrap();
            assert!(revision > passive);
            assert_eq!(parent, 0);
            let summary = "L 99%+ · R 100% · Case 50%";
            let (revision, menu) = menu_with_battery(&bus, summary).await;
            assert!(revision > passive);
            assert_pro_item(&bus, "AirPods Pro", summary).await;
            let labels: Vec<_> = menu.iter().map(|(_, props, _)| label(props)).collect();
            assert_eq!(
                labels,
                [
                    "AirPods Pro",
                    summary,
                    "",
                    "Off",
                    "Noise Cancellation",
                    "Transparency",
                    "Adaptive",
                    "",
                    "Conversation Awareness",
                    "Ear Detection",
                    "One-Bud ANC"
                ]
            );
            assert!(!bool::try_from(&*menu[0].1["enabled"]).unwrap());
            for (text, kind) in [("Off", "radio"), ("Conversation Awareness", "checkmark")] {
                let (_, props, _) = menu
                    .iter()
                    .find(|(_, props, _)| label(props) == text)
                    .unwrap();
                assert_eq!(<&str>::try_from(&*props["toggle-type"]).unwrap(), kind);
            }
            assert_menu_actions(&rig, &device, &bus, &menu).await;
        });
        rig.link_down(&device);
        runtime.block_on(async {
            let passive: String = next_signal(&mut status).await.body().deserialize().unwrap();
            assert_eq!(passive, "Passive");
        });
        rig.finish();
    }

    #[test]
    fn watcher_can_appear_after_the_daemon() {
        let runtime = Runtime::new_current_thread().enable_all().build().unwrap();
        let mut rig = Rig::new();
        let address = rig.session_bus().to_owned();
        rig.start();
        runtime.block_on(async {
            let _bus = owned_connection(&address, rig.daemon()).await;
            tokio::time::sleep(QUIET_WINDOW).await;
            let (_watcher, mut requests) = watcher(&address).await;
            registered(&mut requests).await;
        });
        assert!(!rig.daemon().log().contains("desktop tray:"));
        rig.finish();
    }

    #[test]
    fn tray_respects_overrides_and_desktop_integrations() {
        let runtime = Runtime::new_current_thread().enable_all().build().unwrap();
        for (setting, omarchy, gnome, enabled) in [
            (Some("0"), false, false, false),
            (None, true, false, false),
            (None, false, true, false),
            (Some("1"), true, true, true),
        ] {
            let mut rig = Rig::new();
            if omarchy {
                fs::create_dir_all(
                    rig.session()
                        .config_home()
                        .join("omarchy/plugins/superwhisper.pods"),
                )
                .unwrap();
            }
            let data = rig.session().config_home().join("data");
            if gnome {
                fs::create_dir_all(data.join("gnome-shell/extensions/pods@superwhisper.com"))
                    .unwrap();
            }
            rig.prepare(move |command| {
                command.env("XDG_DATA_HOME", data).env(
                    "XDG_CURRENT_DESKTOP",
                    if gnome { "ubuntu:GNOME" } else { "KDE" },
                );
                if let Some(setting) = setting {
                    command.env("PODS_TRAY", setting);
                }
            });
            let address = rig.session_bus().to_owned();
            let (_watcher, mut requests) = runtime.block_on(watcher(&address));
            rig.start();
            runtime.block_on(async {
                let bus = owned_connection(&address, rig.daemon()).await;
                if enabled {
                    registered(&mut requests).await;
                    assert_eq!(
                        String::try_from(property(&bus, ITEM_PATH, ITEM, "Status").await).unwrap(),
                        "Passive"
                    );
                } else {
                    let reply = bus
                        .call_method(
                            Some(BUS_NAME),
                            ITEM_PATH,
                            Some(PROPERTIES),
                            "Get",
                            &(ITEM, "Status"),
                        )
                        .await;
                    assert!(
                        matches!(reply, Err(zbus::Error::FDO(ref error)) if matches!(**error, zbus::fdo::Error::UnknownObject(_))),
                        "{reply:?}"
                    );
                    assert!(
                        tokio::time::timeout(QUIET_WINDOW, requests.recv())
                            .await
                            .is_err()
                    );
                }
            });
            rig.finish();
        }
    }

    #[test]
    fn plain_buds_hide_noise_controls_and_escape_device_name() {
        let runtime = Runtime::new_current_thread().enable_all().build().unwrap();
        let mut rig = Rig::new();
        let profile = Profile {
            name: "Tom & <Jerry>\n_Pods\r\x07".to_owned(),
            model_number: "A2031".to_owned(),
            ..Profile::default()
        };
        let device = rig.add_pods(profile);
        let address = rig.session_bus().to_owned();
        let (_watcher, mut requests) = runtime.block_on(watcher(&address));
        rig.start();
        let bus = runtime.block_on(owned_connection(&address, rig.daemon()));
        runtime.block_on(registered(&mut requests));
        rig.link_up(&device);
        rig.wait_filled();
        runtime.block_on(async {
            let summary = "L 99%+ · R 100% · Case 50%";
            let (_, menu) = menu_with_battery(&bus, summary).await;
            let labels: Vec<_> = menu.iter().map(|(_, props, _)| label(props)).collect();
            assert_eq!(
                labels,
                ["Tom & <Jerry> __Pods", summary, "", "Ear Detection"]
            );
            let icon = property(&bus, ITEM_PATH, ITEM, "IconName").await;
            assert_eq!(String::try_from(icon).unwrap(), "pods-buds-symbolic");
            let tip = property(&bus, ITEM_PATH, ITEM, "ToolTip").await;
            let (tip_icon, _, tip_title, tip_body): (String, Pixmaps, String, String) =
                tip.try_into().unwrap();
            assert_eq!(tip_icon, "pods-buds-symbolic");
            assert_eq!(tip_title, "Tom &amp; &lt;Jerry&gt; _Pods");
            assert_eq!(tip_body, summary);
        });
        rig.finish();
    }
}
