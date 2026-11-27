//! Desktop tray surfaces on the daemon's session connection.

mod item;
mod menu;

use std::env;
use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use pods_core::state::State;
use tokio::sync::watch;
use zbus::export::futures_core::Stream;
use zbus::{Connection, MessageStream};

use super::{both, hub::Hub, log};
use item::{ITEM_PATH, Item, Presentation};
use menu::{MENU_PATH, Menu};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const REGISTER_TIMEOUT: Duration = Duration::from_secs(2);

fn xdg_home(variable: &str, default: &str) -> Option<PathBuf> {
    env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(default)))
}

async fn enabled() -> bool {
    match env::var("PODS_TRAY").as_deref() {
        Ok("0") => return false,
        Ok("1") => return true,
        _ => {}
    }
    let omarchy_path = xdg_home("XDG_CONFIG_HOME", ".config")
        .map(|path| path.join("omarchy/plugins/superwhisper.pods"));
    let gnome_path = env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktop| desktop.contains("GNOME"))
        .then(|| {
            xdg_home("XDG_DATA_HOME", ".local/share")
                .map(|path| path.join("gnome-shell/extensions/pods@superwhisper.com"))
        })
        .flatten();
    tokio::task::spawn_blocking(move || {
        let omarchy = omarchy_path.is_some_and(|path| path.exists());
        let gnome = gnome_path.is_some_and(|path| path.exists());
        !omarchy && !gnome
    })
    .await
    .unwrap_or(false)
}

pub(super) async fn serve(connection: &Connection, hub: Hub) {
    if enabled().await
        && let Err(error) = run(connection, hub).await
    {
        log(&format!("desktop tray: {error}"));
    }
}

async fn run(connection: &Connection, hub: Hub) -> zbus::Result<()> {
    let mut updates = hub.subscribe();
    let initial = updates.borrow_and_update().clone();
    let objects = connection.object_server();
    objects.at(MENU_PATH, Menu::new(hub, &initial)).await?;
    objects.at(ITEM_PATH, Item::new(&initial)).await?;
    both(
        async {
            if let Err(error) = announce(connection, updates).await {
                log(&format!("desktop tray: cannot announce changes: {error}"));
            }
        },
        watch_host(connection),
    )
    .await;
    Ok(())
}

async fn announce(
    connection: &Connection,
    mut updates: watch::Receiver<State>,
) -> zbus::Result<()> {
    let objects = connection.object_server();
    let item = objects.interface::<_, Item>(ITEM_PATH).await?;
    let menu = objects.interface::<_, Menu>(MENU_PATH).await?;
    while updates.changed().await.is_ok() {
        let state = updates.borrow_and_update().clone();
        let next = Presentation::from(&state);
        let previous = item.get_mut().await.replace(next.clone());
        let revision = menu.get_mut().await.update(&state);
        next.announce(item.signal_emitter(), &previous).await?;
        if let Some(revision) = revision {
            Menu::layout_updated(menu.signal_emitter(), revision, 0).await?;
        }
    }
    Ok(())
}

async fn watch_host(connection: &Connection) {
    if let Err(error) = register_with_watchers(connection).await {
        log(&format!("desktop tray: cannot watch {WATCHER}: {error}"));
    }
}

async fn register_with_watchers(connection: &Connection) -> zbus::Result<()> {
    const BUS: &str = "org.freedesktop.DBus";
    const BUS_PATH: &str = "/org/freedesktop/DBus";
    // Subscribe before checking the current owner so startup cannot miss a watcher.
    let rule = format!(
        "type='signal',sender='{BUS}',path='{BUS_PATH}',interface='{BUS}',member='NameOwnerChanged',arg0='{WATCHER}'"
    );
    let mut changes = MessageStream::for_match_rule(rule.as_str(), connection, None).await?;
    let current = connection
        .call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &(WATCHER,))
        .await;
    let mut registered: String = match current {
        Ok(reply) => reply.body().deserialize()?,
        Err(zbus::Error::FDO(error)) if matches!(*error, zbus::fdo::Error::NameHasNoOwner(_)) => {
            String::new()
        }
        Err(error) => return Err(error),
    };
    if !registered.is_empty() {
        register(connection, &registered).await;
    }
    // Sample input: ("org.kde.StatusNotifierWatcher", ":1.4", ":1.8"); an empty owner means no watcher.
    while let Some(change) = poll_fn(|cx| Pin::new(&mut changes).poll_next(cx)).await {
        let (_, _, owner): (String, String, String) = change?.body().deserialize()?;
        if owner != registered {
            if !owner.is_empty() {
                register(connection, &owner).await;
            }
            registered = owner;
        }
    }
    Ok(())
}

async fn register(connection: &Connection, owner: &str) {
    let call = connection.call_method(
        Some(owner),
        WATCHER_PATH,
        Some(WATCHER),
        "RegisterStatusNotifierItem",
        &(ITEM_PATH,),
    );
    // A watcher can exit after its owner was announced or stop responding.
    drop(tokio::time::timeout(REGISTER_TIMEOUT, call).await);
}
