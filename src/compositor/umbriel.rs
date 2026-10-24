use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::Workspace;

// monitor names keep their slots after unplugging, so an old id never points at another monitor
static OUTPUTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/*
 * umbriel reads one json request per line and answers on the same connection;
 * none when umbriel is not running, like under another compositor
 */
fn send(request: &Value) -> Option<UnixStream> {
    let path = env::var_os("UMBRIEL_SOCKET")?;
    let mut stream = UnixStream::connect(path).ok()?;

    stream.write_all(format!("{request}\n").as_bytes()).ok()?;

    Some(stream)
}

pub fn focus_workspace(id: i64) {
    let Some(arg) = focus_action(id) else {
        return;
    };

    // the reply only says whether the action was taken
    if let Some(stream) = send(&json!({ "cmd": "msg", "arg": arg })) {
        let _ = BufReader::new(stream).read_line(&mut String::new());
    }
}

// "workspace-switch:2/DP-1": the workspace by its place, on its own monitor
fn focus_action(id: i64) -> Option<String> {
    let outputs = OUTPUTS.lock().ok()?;

    let output = outputs.get(usize::try_from(id >> 32).ok()?)?;
    let index = id & 0xffff_ffff;

    // a name that could end the action early isn't sent
    if output.contains(['/', ':', '\n', '\r']) {
        return None;
    }

    Some(format!("workspace-switch:{index}/{output}"))
}

pub fn listen(on_change: impl FnMut(Vec<Workspace>)) {
    let Some(stream) = send(&json!({ "cmd": "subscribe", "events": ["workspaces"] })) else {
        return;
    };

    // the current workspaces come first, then the whole list again on each change
    events(BufReader::new(stream), on_change);
}

fn events(reader: impl BufRead, mut on_change: impl FnMut(Vec<Workspace>)) {
    for line in reader.lines() {
        let Ok(line) = line else {
            return;
        };

        let Ok(mut outputs) = OUTPUTS.lock() else {
            return;
        };

        let list = parse(&line, &mut outputs);

        drop(outputs);

        if let Some(list) = list {
            on_change(list);
        }
    }
}

// one event line, {"event":"workspaces","data":[...]}; none for anything else
fn parse(line: &str, outputs: &mut Vec<String>) -> Option<Vec<Workspace>> {
    let value: Value = serde_json::from_str(line).ok()?;

    if value["event"].as_str() != Some("workspaces") {
        return None;
    }

    let mut list = Vec::new();

    for workspace in value["data"].as_array()? {
        let output = workspace["output"].as_str()?;

        let slot = match outputs.iter().position(|name| name == output) {
            Some(slot) => slot,
            None => {
                outputs.push(output.to_string());
                outputs.len() - 1
            }
        };

        let index = u32::try_from(workspace["index"].as_u64()?).ok()?;

        // an unnamed workspace is called by its number, which the shell shows anyway
        let name = workspace["named"]
            .as_bool()
            .unwrap_or(false)
            .then(|| workspace["name"].as_str().map(String::from))
            .flatten();

        list.push(Workspace {
            id: ((slot as i64) << 32) | i64::from(index),
            index,
            name,
            output: Some(output.to_string()),
            active: workspace["active"].as_bool()?,
            focused: workspace["focused"].as_bool()?,
            urgent: workspace["urgent"].as_bool().unwrap_or(false),
            windows: u32::try_from(workspace["window_count"].as_u64()?).ok()?,
        });
    }

    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    // trimmed from `umbriel subscribe workspaces`
    const EVENT: &str = r#"{"data":[
        {"active":true,"focused":true,"id":"DP-1:1","index":1,"name":"1","named":false,"output":"DP-1","urgent":false,"window_count":2},
        {"active":false,"focused":false,"id":"DP-1:2","index":2,"name":"web","named":true,"output":"DP-1","urgent":true,"window_count":0},
        {"active":true,"focused":false,"id":"DP-2:1","index":1,"name":"1","named":false,"output":"DP-2","urgent":false,"window_count":1}
    ],"event":"workspaces"}"#;

    #[test]
    fn reads_each_monitors_workspaces() {
        let list = parse(EVENT, &mut Vec::new()).expect("failed to parse umbriel workspaces");

        assert_eq!(list.len(), 3);

        assert_eq!(
            (list[0].index, list[0].output.as_deref(), list[0].windows),
            (1, Some("DP-1"), 2)
        );
        assert!(list[0].active && list[0].focused);
        assert_eq!(list[0].name, None);

        assert_eq!(list[1].name.as_deref(), Some("web"));
        assert!(list[1].urgent && !list[1].active);

        assert!(list[2].active && !list[2].focused);
        assert_ne!(list[0].id, list[2].id);
    }

    #[test]
    fn other_events_are_skipped() {
        assert!(parse(r#"{"data":[],"event":"windows"}"#, &mut Vec::new()).is_none());
        assert!(parse("not json", &mut Vec::new()).is_none());
    }

    #[test]
    fn keeps_ids_when_monitors_reorder() {
        let mut outputs = Vec::new();

        let first = parse(EVENT, &mut outputs).unwrap();

        let mut value: Value = serde_json::from_str(EVENT).unwrap();
        value["data"].as_array_mut().unwrap().reverse();

        let reordered = parse(&value.to_string(), &mut outputs).unwrap();

        assert_eq!(first[0].id, reordered[2].id);
        assert_eq!(first[2].id, reordered[0].id);
    }

    #[test]
    fn focuses_by_place_on_its_monitor() {
        let mut outputs = OUTPUTS.lock().unwrap();

        outputs.clear();

        let list = parse(EVENT, &mut outputs).unwrap();

        drop(outputs);

        assert_eq!(
            focus_action(list[1].id).as_deref(),
            Some("workspace-switch:2/DP-1")
        );
        assert_eq!(
            focus_action(list[2].id).as_deref(),
            Some("workspace-switch:1/DP-2")
        );
        assert_eq!(focus_action(99 << 32), None);
    }
}
