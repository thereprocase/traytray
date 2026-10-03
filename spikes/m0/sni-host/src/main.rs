//! M0.1 spike: a read-only StatusNotifierHost running next to plasmashell.
//!
//! Registers with org.kde.StatusNotifierWatcher under a well-known name this
//! process owns, reads every registered item and its dbusmenu layout, then logs
//! watcher and item signals for a fixed window.
//!
//! Read-only by design: the only calls made on items are property reads and
//! com.canonical.dbusmenu.GetLayout. Activate, SecondaryActivate, ContextMenu,
//! Scroll, Event and AboutToShow are never called.
//!
//! With `--test-item`, a second connection publishes a throwaway Passive item
//! so the Registered/NewStatus/NewIcon/Unregistered signals have a known
//! source; otherwise a quiet desktop could make the signal test vacuous.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream, Proxy};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const MENU_IFACE: &str = "com.canonical.dbusmenu";

type Pixmap = Vec<(i32, i32, Vec<u8>)>;
type ToolTip = (String, Pixmap, String, String);
/// dbusmenu layout node, signature (ia{sv}av); children are variants of the same shape.
type LayoutNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn log(msg: impl AsRef<str>) {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    println!("[{t:.3}] {}", msg.as_ref());
}

#[tokio::main]
async fn main() -> zbus::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let listen_secs: u64 = args
        .iter()
        .position(|a| a == "--secs")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let with_test_item = args.iter().any(|a| a == "--test-item");

    let conn = Connection::session().await?;
    let watcher = Proxy::new(&conn, WATCHER_NAME, WATCHER_PATH, WATCHER_NAME).await?;

    let before: Vec<String> = watcher.get_property("RegisteredStatusNotifierItems").await?;
    let host_registered: bool = watcher.get_property("IsStatusNotifierHostRegistered").await?;
    let proto: i32 = watcher.get_property("ProtocolVersion").await?;
    log(format!(
        "watcher: ProtocolVersion={proto} IsStatusNotifierHostRegistered={host_registered} items_before={}",
        before.len()
    ));

    // Subscribe before registering so no signal emitted in response to our
    // registration is missed.
    let mut signals = subscribe(&conn).await?;

    // The spec convention is org.kde.StatusNotifierHost-<pid>; the pid makes it
    // unique to this process.
    let host_name = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    conn.request_name(host_name.as_str()).await?;
    watcher
        .call_method("RegisterStatusNotifierHost", &(host_name.as_str(),))
        .await?;
    log(format!("registered host as {host_name}"));

    let items: Vec<String> = watcher.get_property("RegisteredStatusNotifierItems").await?;
    log(format!("items_after_register={}", items.len()));
    for service in &items {
        dump_item(&conn, service).await;
    }

    let test_item = if with_test_item {
        Some(tokio::spawn(test_item::run()))
    } else {
        None
    };

    log(format!("listening for signals for {listen_secs} s"));
    let deadline = tokio::time::sleep(Duration::from_secs(listen_secs));
    tokio::pin!(deadline);
    let mut seen_registered: Vec<String> = Vec::new();
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            Some(msg) = signals.next() => {
                let Ok(msg) = msg else { continue };
                // The connection stream also carries method returns and bus
                // housekeeping; only signals are of interest here.
                if msg.message_type() != zbus::message::Type::Signal {
                    continue;
                }
                let header = msg.header();
                let member = header.member().map(|m| m.to_string()).unwrap_or_default();
                let iface = header.interface().map(|i| i.to_string()).unwrap_or_default();
                let sender = header.sender().map(|s| s.to_string()).unwrap_or_default();
                let path = header.path().map(|p| p.to_string()).unwrap_or_default();
                let body: String = msg.body().deserialize::<String>().unwrap_or_default();
                log(format!("SIGNAL {iface}.{member} sender={sender} path={path} arg0={body:?}"));
                // KDE's watcher emits each signal on several object paths, so
                // the same item can be announced more than once.
                if member == "StatusNotifierItemRegistered" && !seen_registered.contains(&body) {
                    seen_registered.push(body.clone());
                    // Read the new item right away, while it is still alive.
                    let conn = conn.clone();
                    tokio::spawn(async move { dump_item(&conn, &body).await });
                }
            }
        }
    }

    log(format!("items registered during window: {seen_registered:?}"));

    if let Some(handle) = test_item {
        let _ = handle.await;
    }

    let after: Vec<String> = watcher.get_property("RegisteredStatusNotifierItems").await?;
    log(format!("items_at_end={} {:?}", after.len(), after));

    // Plasmashell's own host name must still be owned after we registered.
    let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
    let names = dbus.list_names().await?;
    let hosts: Vec<String> = names
        .iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with("org.kde.StatusNotifierHost-"))
        .collect();
    log(format!("hosts on bus at end: {hosts:?}"));
    Ok(())
}

async fn subscribe(conn: &Connection) -> zbus::Result<MessageStream> {
    // One stream fed by several match rules: watcher item add/remove and the
    // per-item change notifications from any sender.
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let rules = [
        MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(WATCHER_NAME)?
            .build(),
        MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(ITEM_IFACE)?
            .build(),
    ];
    for rule in rules {
        dbus.add_match_rule(rule).await?;
    }
    Ok(MessageStream::from(conn.clone()))
}

/// Splits a watcher entry ("busname/object/path" or a bare bus name) into
/// destination and object path.
fn split_service(service: &str) -> (String, String) {
    match service.find('/') {
        Some(i) => (service[..i].to_string(), service[i..].to_string()),
        None => (service.to_string(), "/StatusNotifierItem".to_string()),
    }
}

async fn dump_item(conn: &Connection, service: &str) {
    let (dest, path) = split_service(service);
    log(format!("ITEM {service}"));
    let proxy = match Proxy::new(conn, dest.as_str(), path.as_str(), ITEM_IFACE).await {
        Ok(p) => p,
        Err(e) => {
            log(format!("  proxy error: {e}"));
            return;
        }
    };
    for name in ["Id", "Title", "Status", "Category", "IconName", "AttentionIconName"] {
        match proxy.get_property::<String>(name).await {
            Ok(v) => log(format!("  {name} = {v:?}")),
            Err(e) => log(format!("  {name} error: {e}")),
        }
    }
    for name in ["IconPixmap", "AttentionIconPixmap", "OverlayIconPixmap"] {
        match proxy.get_property::<Pixmap>(name).await {
            Ok(v) => {
                let sizes: Vec<String> = v
                    .iter()
                    .map(|(w, h, d)| format!("{w}x{h}({}B)", d.len()))
                    .collect();
                log(format!("  {name} sizes = {sizes:?}"));
            }
            Err(e) => log(format!("  {name} error: {e}")),
        }
    }
    match proxy.get_property::<ToolTip>("ToolTip").await {
        Ok((icon, pix, title, text)) => log(format!(
            "  ToolTip icon={icon:?} pixmaps={} title={title:?} text={text:?}",
            pix.len()
        )),
        Err(e) => log(format!("  ToolTip error: {e}")),
    }
    match proxy.get_property::<bool>("ItemIsMenu").await {
        Ok(v) => log(format!("  ItemIsMenu = {v}")),
        Err(e) => log(format!("  ItemIsMenu error: {e}")),
    }
    let menu = match proxy.get_property::<OwnedObjectPath>("Menu").await {
        Ok(p) => p,
        Err(e) => {
            log(format!("  Menu error: {e}"));
            return;
        }
    };
    log(format!("  Menu = {}", menu.as_str()));
    dump_menu(conn, &dest, menu.as_str()).await;
}

async fn dump_menu(conn: &Connection, dest: &str, path: &str) {
    let proxy = match Proxy::new(conn, dest, path, MENU_IFACE).await {
        Ok(p) => p,
        Err(e) => {
            log(format!("  menu proxy error: {e}"));
            return;
        }
    };
    // GetLayout(parentId=0, recursionDepth=-1 (all), propertyNames=[] (all)).
    let empty: Vec<&str> = Vec::new();
    let reply = proxy
        .call_method("GetLayout", &(0i32, -1i32, empty))
        .await;
    let msg = match reply {
        Ok(m) => m,
        Err(e) => {
            log(format!("  GetLayout error: {e}"));
            return;
        }
    };
    let body = msg.body();
    let parsed: zbus::Result<(u32, LayoutNode)> = body.deserialize().map_err(Into::into);
    match parsed {
        Ok((revision, (id, props, children))) => {
            log(format!("  GetLayout revision={revision}"));
            let mut count = 0usize;
            let root = Value::Structure(
                zbus::zvariant::StructureBuilder::new()
                    .add_field(id)
                    .add_field(props)
                    .add_field(children)
                    .build()
                    .expect("layout root"),
            );
            walk_layout(&root, 0, &mut count);
            log(format!("  menu entries (excluding root) = {count}"));
        }
        Err(e) => log(format!("  GetLayout parse error: {e}")),
    }
}

/// Walks a dbusmenu layout node, (ia{sv}av), printing one line per entry.
fn walk_layout(node: &Value<'_>, depth: usize, count: &mut usize) {
    let node = match node {
        Value::Value(inner) => inner.as_ref(),
        other => other,
    };
    let Value::Structure(s) = node else {
        log(format!("  {:indent$}<unexpected node {node:?}>", "", indent = depth * 2));
        return;
    };
    let fields = s.fields();
    let id = match fields.first() {
        Some(Value::I32(i)) => *i,
        _ => -1,
    };
    let props: HashMap<String, OwnedValue> = fields
        .get(1)
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| HashMap::<String, OwnedValue>::try_from(v).ok())
        .unwrap_or_default();
    if depth > 0 {
        *count += 1;
        let label = props
            .get("label")
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default();
        let kind = props
            .get("type")
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_else(|| "standard".into());
        let enabled = props
            .get("enabled")
            .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
            .unwrap_or(true);
        let mut keys: Vec<&String> = props.keys().collect();
        keys.sort();
        log(format!(
            "  {:indent$}- id={id} type={kind} enabled={enabled} label={label:?} props={keys:?}",
            "",
            indent = depth * 2
        ));
    }
    if let Some(Value::Array(children)) = fields.get(2) {
        for child in children.iter() {
            walk_layout(child, depth + 1, count);
        }
    }
}

mod test_item {
    //! A throwaway, Passive-status item with a two-entry menu. Passive keeps it
    //! out of the visible panel area; it exists only to give the host signals
    //! from a known source.

    use std::collections::HashMap;
    use std::time::Duration;

    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, StructureBuilder, Value};
    use zbus::{interface, Connection};

    use super::log;

    struct Item {
        status: String,
        icon: String,
    }

    #[interface(name = "org.kde.StatusNotifierItem")]
    impl Item {
        #[zbus(property)]
        fn id(&self) -> String {
            "traytray-spike-test-item".into()
        }
        #[zbus(property)]
        fn title(&self) -> String {
            "Traytray spike test item".into()
        }
        #[zbus(property)]
        fn category(&self) -> String {
            "ApplicationStatus".into()
        }
        #[zbus(property)]
        fn status(&self) -> String {
            self.status.clone()
        }
        #[zbus(property)]
        fn icon_name(&self) -> String {
            self.icon.clone()
        }
        #[zbus(property)]
        fn item_is_menu(&self) -> bool {
            false
        }
        #[zbus(property)]
        fn menu(&self) -> OwnedObjectPath {
            OwnedObjectPath::try_from("/MenuBar").expect("static path")
        }
        #[zbus(signal)]
        async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    }

    struct Menu;

    fn entry(id: i32, label: &str) -> OwnedValue {
        let mut props: HashMap<String, OwnedValue> = HashMap::new();
        props.insert("label".into(), OwnedValue::from(zbus::zvariant::Str::from(label.to_string())));
        let children: Vec<OwnedValue> = Vec::new();
        OwnedValue::try_from(Value::Structure(
            StructureBuilder::new()
                .add_field(id)
                .add_field(props)
                .add_field(children)
                .build()
                .expect("static structure"),
        ))
        .expect("no fds")
    }

    #[interface(name = "com.canonical.dbusmenu")]
    impl Menu {
        #[zbus(property)]
        fn version(&self) -> u32 {
            3
        }
        fn get_layout(
            &self,
            _parent_id: i32,
            _recursion_depth: i32,
            _property_names: Vec<String>,
        ) -> (u32, super::LayoutNode) {
            let children = vec![entry(1, "Spike entry one"), entry(2, "Spike entry two")];
            (1, (0, HashMap::new(), children))
        }
    }

    pub async fn run() {
        if let Err(e) = run_inner().await {
            log(format!("test item error: {e}"));
        }
    }

    async fn run_inner() -> zbus::Result<()> {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let conn = Connection::session().await?;
        let name = format!("org.kde.StatusNotifierItem-{}-99", std::process::id());
        conn.object_server()
            .at(
                "/StatusNotifierItem",
                Item { status: "Passive".into(), icon: "dialog-information".into() },
            )
            .await?;
        conn.object_server().at("/MenuBar", Menu).await?;
        conn.request_name(name.as_str()).await?;
        let watcher = zbus::Proxy::new(
            &conn,
            super::WATCHER_NAME,
            super::WATCHER_PATH,
            super::WATCHER_NAME,
        )
        .await?;
        watcher
            .call_method("RegisterStatusNotifierItem", &(name.as_str(),))
            .await?;
        log(format!("test item registered as {name}"));

        tokio::time::sleep(Duration::from_secs(3)).await;
        let iface = conn
            .object_server()
            .interface::<_, Item>("/StatusNotifierItem")
            .await?;
        iface.get_mut().await.icon = "dialog-warning".into();
        Item::new_icon(iface.signal_emitter()).await?;
        log("test item emitted NewIcon");

        tokio::time::sleep(Duration::from_secs(2)).await;
        // Stay Passive so the item never moves into the visible tray; the
        // signal still carries a status string.
        Item::new_status(iface.signal_emitter(), "Passive").await?;
        log("test item emitted NewStatus(Passive)");

        tokio::time::sleep(Duration::from_secs(3)).await;
        // Dropping the connection releases the name; the watcher is expected
        // to notice and emit StatusNotifierItemUnregistered.
        drop(iface);
        conn.close().await?;
        log("test item connection closed");
        Ok(())
    }
}
