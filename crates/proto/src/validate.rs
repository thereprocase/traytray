//! Validation and normalisation of what apps publish.
//!
//! The core runs every state document and event through here before storing or forwarding
//! it. The result is a cleaned copy: strings sanitised and capped, structure checked against
//! the limits. Anything structurally wrong is rejected whole rather than partly accepted, so
//! an app never sees half its state rendered.

use std::collections::HashSet;

use crate::frames::{ActionDef, AppState, Block, Confirm, EventFrame, MenuItem, Row, Tier};
use crate::limits::*;
use crate::sanitize::{clean, valid_id};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    TooManyItems(usize),
    TooManyBlocks(usize),
    MenuTooDeep,
    BadId(String),
    DuplicateId(String),
    BadNumber(&'static str),
    TierNotGranted(Tier),
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Invalid::TooManyItems(n) => write!(f, "{n} items exceeds the limit of {MAX_ITEMS}"),
            Invalid::TooManyBlocks(n) => write!(f, "{n} blocks exceeds the limit of {MAX_BLOCKS}"),
            Invalid::MenuTooDeep => write!(f, "menu nested deeper than {MAX_MENU_DEPTH}"),
            Invalid::BadId(id) => write!(f, "invalid id {id:?} (printable ASCII, 1-{MAX_ID_CHARS} chars)"),
            Invalid::DuplicateId(id) => write!(f, "duplicate item id {id:?}"),
            Invalid::BadNumber(what) => write!(f, "{what} must be a finite number in 0..=1"),
            Invalid::TierNotGranted(t) => write!(f, "tier {t:?} was not granted to this app"),
        }
    }
}

/// Validate and clean a state document. `granted` restricts which tiers may be used; local
/// apps get all of them.
pub fn state(doc: &AppState, granted: &[Tier]) -> Result<AppState, Invalid> {
    if !doc.menu.is_empty() && !granted.contains(&Tier::Menu) {
        return Err(Invalid::TierNotGranted(Tier::Menu));
    }
    if !doc.blocks.is_empty() && !granted.contains(&Tier::Widgets) {
        return Err(Invalid::TierNotGranted(Tier::Widgets));
    }
    if doc.panel_url.is_some() && !granted.contains(&Tier::Panel) {
        return Err(Invalid::TierNotGranted(Tier::Panel));
    }
    if doc.blocks.len() > MAX_BLOCKS {
        return Err(Invalid::TooManyBlocks(doc.blocks.len()));
    }

    let mut items = 0usize;
    let mut ids = HashSet::new();
    let menu = doc
        .menu
        .iter()
        .map(|m| menu_item(m, 1, &mut items))
        .collect::<Result<Vec<_>, _>>()?;
    let blocks = doc
        .blocks
        .iter()
        .map(|b| block(b, &mut items, &mut ids))
        .collect::<Result<Vec<_>, _>>()?;
    if items > MAX_ITEMS {
        return Err(Invalid::TooManyItems(items));
    }

    Ok(AppState {
        icon: doc.icon.as_deref().map(|s| clean(s, MAX_LABEL_CHARS, false)),
        icon_mark: doc.icon_mark,
        menu,
        blocks,
        // Tier 2 is beta; alpha hosts keep the field only if granted, never act on it.
        panel_url: doc.panel_url.clone(),
    })
}

fn menu_item(m: &MenuItem, depth: usize, items: &mut usize) -> Result<MenuItem, Invalid> {
    if depth > MAX_MENU_DEPTH {
        return Err(Invalid::MenuTooDeep);
    }
    *items += 1;
    if let Some(a) = &m.action_id {
        id(a)?;
    }
    Ok(MenuItem {
        label: clean(&m.label, MAX_LABEL_CHARS, false),
        action_id: m.action_id.clone(),
        checked: m.checked,
        enabled: m.enabled,
        confirm: m.confirm.as_ref().map(confirm),
        submenu: m
            .submenu
            .iter()
            .map(|s| menu_item(s, depth + 1, items))
            .collect::<Result<_, _>>()?,
    })
}

fn block(b: &Block, items: &mut usize, ids: &mut HashSet<String>) -> Result<Block, Invalid> {
    Ok(match b {
        Block::Status { id: i, text, urgency, privacy } => {
            unique(i, ids)?;
            *items += 1;
            Block::Status {
                id: i.clone(),
                text: clean(text, MAX_TITLE_CHARS, false),
                urgency: *urgency,
                privacy: *privacy,
            }
        }
        Block::Text { text } => Block::Text { text: clean(text, MAX_TEXT_CHARS, true) },
        Block::Progress { id: i, label, value, eta_secs, urgency, privacy } => {
            unique(i, ids)?;
            *items += 1;
            Block::Progress {
                id: i.clone(),
                label: clean(label, MAX_TITLE_CHARS, false),
                value: fraction(*value, "progress value")?,
                eta_secs: *eta_secs,
                urgency: *urgency,
                privacy: *privacy,
            }
        }
        Block::List { id: i, rows } => {
            unique(i, ids)?;
            Block::List {
                id: i.clone(),
                rows: rows.iter().map(|r| row(r, items, ids)).collect::<Result<_, _>>()?,
            }
        }
        Block::Buttons { actions } => {
            *items += actions.len();
            Block::Buttons { actions: actions.iter().map(action).collect::<Result<_, _>>()? }
        }
        Block::Reply { item_id, placeholder } => {
            id(item_id)?;
            Block::Reply {
                item_id: item_id.clone(),
                placeholder: clean(placeholder, MAX_LABEL_CHARS, false),
            }
        }
    })
}

fn row(r: &Row, items: &mut usize, ids: &mut HashSet<String>) -> Result<Row, Invalid> {
    unique(&r.id, ids)?;
    *items += 1 + r.actions.len();
    Ok(Row {
        id: r.id.clone(),
        title: clean(&r.title, MAX_TITLE_CHARS, false),
        subtitle: clean(&r.subtitle, MAX_TITLE_CHARS, false),
        badge: clean(&r.badge, MAX_LABEL_CHARS, false),
        icon: r.icon.as_deref().map(|s| clean(s, MAX_LABEL_CHARS, false)),
        progress: fraction(r.progress, "row progress")?,
        urgency: r.urgency,
        privacy: r.privacy,
        actions: r.actions.iter().map(action).collect::<Result<_, _>>()?,
    })
}

fn action(a: &ActionDef) -> Result<ActionDef, Invalid> {
    id(&a.id)?;
    Ok(ActionDef {
        id: a.id.clone(),
        label: clean(&a.label, MAX_LABEL_CHARS, false),
        confirm: a.confirm.as_ref().map(confirm),
        enabled: a.enabled,
    })
}

fn confirm(c: &Confirm) -> Confirm {
    Confirm {
        title: clean(&c.title, MAX_TITLE_CHARS, false),
        body: clean(&c.body, MAX_TEXT_CHARS, true),
        verb: clean(&c.verb, MAX_LABEL_CHARS, false),
    }
}

/// Validate and clean an event.
pub fn event(e: &EventFrame) -> Result<EventFrame, Invalid> {
    id(&e.event_id)?;
    if let Some(i) = &e.item_id {
        id(i)?;
    }
    Ok(EventFrame {
        event_id: e.event_id.clone(),
        urgency: e.urgency,
        title: clean(&e.title, MAX_TITLE_CHARS, false),
        body: clean(&e.body, MAX_TEXT_CHARS, true),
        item_id: e.item_id.clone(),
        toast: e.toast,
        privacy: e.privacy,
    })
}

fn id(s: &str) -> Result<(), Invalid> {
    if valid_id(s) { Ok(()) } else { Err(Invalid::BadId(clean(s, MAX_ID_CHARS, false))) }
}

fn unique(s: &str, ids: &mut HashSet<String>) -> Result<(), Invalid> {
    id(s)?;
    if ids.insert(s.to_owned()) { Ok(()) } else { Err(Invalid::DuplicateId(s.to_owned())) }
}

fn fraction(v: Option<f64>, what: &'static str) -> Result<Option<f64>, Invalid> {
    match v {
        Some(x) if !x.is_finite() || !(0.0..=1.0).contains(&x) => Err(Invalid::BadNumber(what)),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{Privacy, Urgency};

    const ALL: &[Tier] = &[Tier::Menu, Tier::Widgets, Tier::Panel];

    fn row(id: &str) -> Row {
        Row {
            id: id.into(),
            title: "t".into(),
            subtitle: String::new(),
            badge: String::new(),
            icon: None,
            progress: None,
            urgency: Urgency::Quiet,
            privacy: Privacy::default(),
            actions: vec![],
        }
    }

    fn list(rows: Vec<Row>) -> AppState {
        AppState { blocks: vec![Block::List { id: "jobs".into(), rows }], ..Default::default() }
    }

    #[test]
    fn accepts_exactly_the_item_limit_and_rejects_one_more() {
        let ok = list((0..MAX_ITEMS).map(|i| row(&format!("r{i}"))).collect());
        assert!(state(&ok, ALL).is_ok());
        let too_many = list((0..=MAX_ITEMS).map(|i| row(&format!("r{i}"))).collect());
        assert_eq!(state(&too_many, ALL), Err(Invalid::TooManyItems(MAX_ITEMS + 1)));
    }

    #[test]
    fn row_actions_count_toward_the_limit() {
        let mut r = row("r");
        r.actions = (0..MAX_ITEMS)
            .map(|i| ActionDef { id: format!("a{i}"), label: "x".into(), confirm: None, enabled: true })
            .collect();
        assert!(matches!(state(&list(vec![r]), ALL), Err(Invalid::TooManyItems(_))));
    }

    #[test]
    fn duplicate_ids_across_blocks_are_rejected() {
        let mut doc = list(vec![row("same")]);
        doc.blocks.push(Block::Status {
            id: "same".into(),
            text: "x".into(),
            urgency: Urgency::Quiet,
            privacy: Privacy::default(),
        });
        assert_eq!(state(&doc, ALL), Err(Invalid::DuplicateId("same".into())));
    }

    #[test]
    fn menu_depth_is_capped() {
        let mut m = MenuItem {
            label: "leaf".into(),
            action_id: Some("x".into()),
            checked: None,
            enabled: true,
            confirm: None,
            submenu: vec![],
        };
        for _ in 0..MAX_MENU_DEPTH {
            let child = m.clone();
            m = MenuItem { label: "p".into(), action_id: None, submenu: vec![child], ..m };
        }
        let doc = AppState { menu: vec![m], ..Default::default() };
        assert_eq!(state(&doc, ALL), Err(Invalid::MenuTooDeep));
    }

    #[test]
    fn ungranted_tiers_are_rejected() {
        let doc = list(vec![row("r")]);
        assert_eq!(state(&doc, &[Tier::Menu]), Err(Invalid::TierNotGranted(Tier::Widgets)));
        let panel = AppState { panel_url: Some("http://127.0.0.1:1".into()), ..Default::default() };
        assert_eq!(state(&panel, &[Tier::Menu, Tier::Widgets]), Err(Invalid::TierNotGranted(Tier::Panel)));
    }

    #[test]
    fn progress_must_be_a_fraction() {
        let mut r = row("r");
        r.progress = Some(f64::NAN);
        assert!(matches!(state(&list(vec![r.clone()]), ALL), Err(Invalid::BadNumber(_))));
        r.progress = Some(1.5);
        assert!(matches!(state(&list(vec![r.clone()]), ALL), Err(Invalid::BadNumber(_))));
        r.progress = Some(1.0);
        assert!(state(&list(vec![r]), ALL).is_ok());
    }

    #[test]
    fn strings_are_cleaned_in_the_output() {
        let mut r = row("r");
        r.title = "\u{1b}[31mWindows\u{202E} Security".into();
        let out = state(&list(vec![r]), ALL).unwrap();
        let Block::List { rows, .. } = &out.blocks[0] else { panic!() };
        assert_eq!(rows[0].title, "Windows Security");
    }

    #[test]
    fn event_ids_are_checked() {
        let e = EventFrame {
            event_id: "bad id".into(),
            urgency: Urgency::Notice,
            title: "x".into(),
            body: String::new(),
            item_id: None,
            toast: false,
            privacy: Privacy::default(),
        };
        assert!(matches!(event(&e), Err(Invalid::BadId(_))));
    }
}
