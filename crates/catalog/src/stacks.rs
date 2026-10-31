//! Stacks (G36): photographs grouped under one top photograph, as
//! Lightroom Classic stacks them. Library data only (schema 22: `stacks`,
//! `stack_members`); no file, sidecar or XMP field is written for a stack.
//!
//! A member is a photograph (`copy` empty) or one of its virtual copies; a
//! member is in at most one stack. Members are ordered by `position`, the
//! first being the top. Every command runs in one transaction and returns a
//! [`StackChange`]: the touched stacks as they were and as they are, which
//! [`Library::restore_stacks`] puts back (undo) or forward again (redo).
//!
//! Auto-Stack ([`Library::auto_stack_plan`]) groups photographs by camera
//! burst/bracket data where the capture facts hold it (rules below), else by
//! the gap between capture times; it never takes a photograph that is in a
//! stack already.
use crate::Library;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use uuid::Uuid;

/// A photograph (`copy` empty) or one of its virtual copies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub asset_id: String,
    #[serde(default)]
    pub copy: String,
}

impl Member {
    pub fn photo(asset_id: impl Into<String>) -> Self {
        Self {
            asset_id: asset_id.into(),
            copy: String::new(),
        }
    }
}

/// A stack, its top first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stack {
    pub id: String,
    pub collapsed: bool,
    pub members: Vec<Member>,
}

/// The stacks a command touched, as they were and as they are now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackChange {
    pub before: Vec<Stack>,
    pub after: Vec<Stack>,
}

impl StackChange {
    pub fn is_empty(&self) -> bool {
        self.before == self.after
    }
}

/// A stack member with its file's path, as the app lists stacks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedMember {
    pub asset_id: String,
    pub copy: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedStack {
    pub id: String,
    pub collapsed: bool,
    pub members: Vec<ListedMember>,
}

/// How Auto-Stack groups (`Library::auto_stack_plan`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoStackOptions {
    /// Photographs taken at most this many seconds after the one before
    /// share a stack.
    pub interval_secs: u32,
    /// Group camera bursts and brackets by their own data first.
    pub use_camera_data: bool,
}

/// Why Auto-Stack made a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoKind {
    /// Frames of one auto-exposure bracket (EXIF ExposureMode 2).
    Bracket,
    /// Frames of one continuous-drive burst (the maker's frame numbers).
    Burst,
    /// Capture times at most the interval apart.
    Time,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoGroup {
    pub kind: AutoKind,
    /// Photographs (asset ids), the top first.
    pub assets: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoPlan {
    pub groups: Vec<AutoGroup>,
    /// Photographs the groups hold.
    pub photos: usize,
    /// Photographs looked at (not in a stack, with a capture time).
    pub considered: usize,
}

/// One photograph's facts as Auto-Stack reads them.
#[derive(Debug, Clone)]
struct Frame {
    id: String,
    /// Seconds since the epoch of its local wall time.
    time: i64,
    camera: String,
    exposure: Option<f64>,
    mode: Option<u8>,
    bias: Option<f64>,
    sequence: Option<u32>,
}

/// Longest pause (s) between two frames of one bracket or burst, beyond
/// the earlier frame's exposure time: a camera fires a bracket or burst
/// without pause, so a longer one means the photographer started again.
const CAMERA_GAP_SECS: f64 = 10.0;

fn read_stack(conn: &Connection, id: &str) -> Result<Option<Stack>> {
    let Some(collapsed) = conn
        .query_row("SELECT collapsed FROM stacks WHERE id=?", [id], |r| {
            r.get::<_, bool>(0)
        })
        .optional()?
    else {
        return Ok(None);
    };
    let members = conn
        .prepare_cached(
            "SELECT asset_id,copy FROM stack_members WHERE stack_id=? ORDER BY position,rowid",
        )?
        .query_map([id], |r| {
            Ok(Member {
                asset_id: r.get(0)?,
                copy: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(Stack {
        id: id.to_string(),
        collapsed,
        members,
    }))
}

fn stack_of(conn: &Connection, member: &Member) -> Result<Option<String>> {
    Ok(conn
        .prepare_cached("SELECT stack_id FROM stack_members WHERE asset_id=? AND copy=?")?
        .query_row(params![member.asset_id, member.copy], |r| r.get(0))
        .optional()?)
}

/// The ids of the stacks `members` are in, in the members' order, once each.
fn stacks_of(conn: &Connection, members: &[Member]) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for member in members {
        if let Some(id) = stack_of(conn, member)?
            && seen.insert(id.clone())
        {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn read_stacks(conn: &Connection, ids: &[String]) -> Result<Vec<Stack>> {
    let mut stacks = Vec::new();
    for id in ids {
        if let Some(stack) = read_stack(conn, id)? {
            stacks.push(stack);
        }
    }
    Ok(stacks)
}

/// Whether `member` is a registered photograph or recorded virtual copy.
fn exists(conn: &Connection, member: &Member) -> Result<bool> {
    let found = if member.copy.is_empty() {
        conn.prepare_cached("SELECT 1 FROM assets WHERE id=?")?
            .query_row([&member.asset_id], |_| Ok(()))
            .optional()?
    } else {
        conn.prepare_cached("SELECT 1 FROM versions WHERE asset_id=? AND name=? AND is_primary=0")?
            .query_row(params![member.asset_id, member.copy], |_| Ok(()))
            .optional()?
    };
    Ok(found.is_some())
}

/// Writes `stack` (replacing a stack of its id), leaving out members that
/// are gone; a stack left with fewer than two is not written.
fn write_stack(conn: &Connection, stack: &Stack) -> Result<()> {
    conn.execute("DELETE FROM stacks WHERE id=?", [&stack.id])?;
    let mut members = Vec::new();
    let mut seen = HashSet::new();
    for member in &stack.members {
        if seen.insert(member) && exists(conn, member)? {
            members.push(member);
        }
    }
    if members.len() < 2 {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO stacks(id,collapsed) VALUES(?,?)",
        params![stack.id, stack.collapsed],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO stack_members(stack_id,asset_id,copy,position) VALUES(?,?,?,?)",
    )?;
    for (position, member) in members.iter().enumerate() {
        // A member still in another stack leaves it (its triggers dissolve
        // a stack left with one member).
        conn.execute(
            "DELETE FROM stack_members WHERE asset_id=? AND copy=?",
            params![member.asset_id, member.copy],
        )?;
        insert.execute(params![
            stack.id,
            member.asset_id,
            member.copy,
            i64::try_from(position)?
        ])?;
    }
    Ok(())
}

impl Library {
    /// Runs `change` on the stacks of `members` (and those it names by id
    /// afterwards) in one transaction; returns them before and after.
    fn stack_command(
        &self,
        members: &[Member],
        change: impl FnOnce(&Connection, &[String]) -> Result<Vec<String>>,
    ) -> Result<StackChange> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let touched = stacks_of(&tx, members)?;
        let before = read_stacks(&tx, &touched)?;
        let made = change(&tx, &touched)?;
        let mut ids = touched;
        for id in made {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let after = read_stacks(&tx, &ids)?;
        tx.commit()?;
        Ok(StackChange { before, after })
    }

    /// Group into Stack: `members` become one stack, the first given its
    /// top (the active photograph). A member already in a stack brings its
    /// whole stack along, in its order, at its place (Lightroom merges
    /// stacks so). The new stack is collapsed. Fewer than two members:
    /// nothing changes.
    pub fn group_stack(&self, members: &[Member]) -> Result<StackChange> {
        self.stack_command(members, |conn, touched| {
            let old: HashMap<String, Stack> = read_stacks(conn, touched)?
                .into_iter()
                .map(|s| (s.id.clone(), s))
                .collect();
            let mut order = Vec::new();
            let mut seen = HashSet::new();
            for member in members {
                if !exists(conn, member)? {
                    continue;
                }
                let joined = match stack_of(conn, member)?.and_then(|id| old.get(&id)) {
                    Some(stack) => stack.members.clone(),
                    None => vec![member.clone()],
                };
                for m in joined {
                    if seen.insert(m.clone()) {
                        order.push(m);
                    }
                }
            }
            if order.len() < 2 {
                return Ok(Vec::new());
            }
            for id in touched {
                conn.execute("DELETE FROM stacks WHERE id=?", [id])?;
            }
            let stack = Stack {
                id: Uuid::new_v4().to_string(),
                collapsed: true,
                members: order,
            };
            write_stack(conn, &stack)?;
            Ok(vec![stack.id])
        })
    }

    /// Unstack: dissolves every stack one of `members` is in.
    pub fn unstack(&self, members: &[Member]) -> Result<StackChange> {
        self.stack_command(members, |conn, touched| {
            for id in touched {
                conn.execute("DELETE FROM stacks WHERE id=?", [id])?;
            }
            Ok(Vec::new())
        })
    }

    /// Remove from Stack: `members` leave their stacks (the next member
    /// becomes the top of a stack whose top left; a stack left with one
    /// member is dissolved).
    pub fn remove_from_stack(&self, members: &[Member]) -> Result<StackChange> {
        self.stack_command(members, |conn, _| {
            for member in members {
                conn.execute(
                    "DELETE FROM stack_members WHERE asset_id=? AND copy=?",
                    params![member.asset_id, member.copy],
                )?;
            }
            Ok(Vec::new())
        })
    }

    /// Move to Top of Stack: `member` becomes its stack's top.
    pub fn move_to_stack_top(&self, member: &Member) -> Result<StackChange> {
        self.stack_command(std::slice::from_ref(member), |conn, _| {
            conn.execute(
                "UPDATE stack_members SET position=(SELECT min(position)-1 FROM stack_members s
                   WHERE s.stack_id=stack_members.stack_id)
                 WHERE asset_id=? AND copy=?",
                params![member.asset_id, member.copy],
            )?;
            Ok(Vec::new())
        })
    }

    /// Collapses (`Some(true)`), expands (`Some(false)`) or toggles
    /// (`None`, each stack on its own) the stacks `members` are in.
    pub fn set_stacks_collapsed(
        &self,
        members: &[Member],
        collapsed: Option<bool>,
    ) -> Result<StackChange> {
        self.stack_command(members, |conn, touched| {
            for id in touched {
                match collapsed {
                    Some(value) => conn.execute(
                        "UPDATE stacks SET collapsed=? WHERE id=?",
                        params![value, id],
                    )?,
                    None => {
                        conn.execute("UPDATE stacks SET collapsed=1-collapsed WHERE id=?", [id])?
                    }
                };
            }
            Ok(Vec::new())
        })
    }

    /// Puts the stacks of `change` back as they were (`undo`) or as they
    /// became (redo): the stacks of the other side are removed, those of
    /// this side written again (members since removed from the library
    /// left out, members since stacked elsewhere taken back).
    pub fn restore_stacks(&self, change: &StackChange, undo: bool) -> Result<()> {
        let (target, other) = if undo {
            (&change.before, &change.after)
        } else {
            (&change.after, &change.before)
        };
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for stack in other.iter().chain(target) {
            tx.execute("DELETE FROM stacks WHERE id=?", [&stack.id])?;
        }
        for stack in target {
            write_stack(&tx, stack)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every stack with its members' paths (a copy's path is its
    /// photograph's).
    pub fn listed_stacks(&self) -> Result<Vec<ListedStack>> {
        let rows: Vec<(String, bool, String, String, String, String, bool)> = self
            .conn
            .prepare(
                "SELECT s.id,s.collapsed,m.asset_id,m.copy,a.rel_path,r.path,r.external
                 FROM stacks s JOIN stack_members m ON m.stack_id=s.id
                 JOIN assets a ON a.id=m.asset_id JOIN roots r ON r.id=a.root_id
                 ORDER BY s.rowid,m.position,m.rowid",
            )?
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut stacks: Vec<ListedStack> = Vec::new();
        for (id, collapsed, asset_id, copy, relative, root, external) in rows {
            if stacks.last().is_none_or(|s| s.id != id) {
                stacks.push(ListedStack {
                    id,
                    collapsed,
                    members: Vec::new(),
                });
            }
            let path = self.join_root(root, external, relative);
            if let Some(stack) = stacks.last_mut() {
                stack.members.push(ListedMember {
                    asset_id,
                    copy,
                    path,
                });
            }
        }
        Ok(stacks)
    }

    /// The stack `member` is in.
    pub fn stack_of(&self, member: &Member) -> Result<Option<Stack>> {
        match stack_of(&self.conn, member)? {
            Some(id) => read_stack(&self.conn, &id),
            None => Ok(None),
        }
    }

    /// Auto-Stack by Capture Time: how the photographs `assets` (asset
    /// ids; those in a stack, or with a member copy in one, and those
    /// without a capture time are left out) would be grouped. Writes
    /// nothing.
    ///
    /// With `use_camera_data`, frames the camera marked are grouped by that
    /// first, each group a stack of its own however close other photographs
    /// were taken:
    /// - Bracket: consecutive frames of one camera with EXIF ExposureMode 2
    ///   (auto bracket; written by Canon, Nikon, Sony, Fujifilm, Olympus/OM,
    ///   Panasonic, Pentax and others), at most `CAMERA_GAP_SECS` beyond
    ///   the earlier frame's exposure apart; a frame repeating an exposure
    ///   bias already in the bracket starts the next bracket. Its top is the
    ///   frame nearest 0 EV (the earliest on a tie).
    /// - Burst: consecutive frames of one camera whose maker frame number
    ///   (`sequence`, read where the facts reader knows the maker's field)
    ///   counts up by one from the frame before, as close in time. Its top
    ///   is its first frame.
    ///
    /// The rest are grouped by capture time: a photograph taken at most
    /// `interval_secs` after the one before joins its group (one camera or
    /// several, Lightroom's rule). The effective capture time is used (a
    /// Shift Capture Time correction counts); ties are ordered by file
    /// name.
    pub fn auto_stack_plan(
        &self,
        assets: &[String],
        options: AutoStackOptions,
    ) -> Result<AutoPlan> {
        let ids = serde_json::to_string(assets)?;
        let effective = crate::capture_time::EFFECTIVE;
        let mut frames: Vec<(Frame, String)> = self
            .conn
            .prepare(&format!(
                "SELECT a.id,{effective},a.make||' '||a.camera,a.exposure,a.exposure_mode,
                        a.exposure_bias,a.sequence,a.rel_path
                 FROM assets a JOIN (SELECT DISTINCT value AS id FROM json_each(?)) j ON j.id=a.id
                 WHERE NOT EXISTS(SELECT 1 FROM stack_members m WHERE m.asset_id=a.id)"
            ))?
            .query_map([ids], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                    r.get::<_, Option<u8>>(4)?,
                    r.get::<_, Option<f64>>(5)?,
                    r.get::<_, Option<u32>>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })?
            .filter_map(|row| {
                let (id, time, camera, exposure, mode, bias, sequence, name) = match row {
                    Ok(row) => row,
                    Err(e) => return Some(Err(e)),
                };
                let time =
                    chrono::NaiveDateTime::parse_from_str(time.as_deref()?, "%Y-%m-%d %H:%M:%S")
                        .ok()?
                        .and_utc()
                        .timestamp();
                Some(Ok((
                    Frame {
                        id,
                        time,
                        camera,
                        exposure,
                        mode,
                        bias,
                        sequence,
                    },
                    name,
                )))
            })
            .collect::<rusqlite::Result<_>>()?;
        frames.sort_by(|(a, an), (b, bn)| a.time.cmp(&b.time).then_with(|| an.cmp(bn)));
        let frames: Vec<Frame> = frames.into_iter().map(|(f, _)| f).collect();
        let considered = frames.len();
        let groups = auto_groups(&frames, options);
        Ok(AutoPlan {
            photos: groups.iter().map(|g| g.assets.len()).sum(),
            groups,
            considered,
        })
    }

    /// Makes the stacks of `groups` (from `auto_stack_plan`), each collapsed,
    /// in one transaction; a photograph stacked meanwhile is left out.
    pub fn auto_stack(&self, groups: &[AutoGroup]) -> Result<StackChange> {
        let tx = rusqlite::Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut after = Vec::new();
        for group in groups {
            let mut members = Vec::new();
            for id in &group.assets {
                let member = Member::photo(id.clone());
                let stacked: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM stack_members WHERE asset_id=?)",
                    [id],
                    |r| r.get(0),
                )?;
                if !stacked && !members.contains(&member) {
                    members.push(member);
                }
            }
            let stack = Stack {
                id: Uuid::new_v4().to_string(),
                collapsed: true,
                members,
            };
            write_stack(&tx, &stack)?;
            if let Some(stack) = read_stack(&tx, &stack.id)? {
                after.push(stack);
            }
        }
        tx.commit()?;
        Ok(StackChange {
            before: Vec::new(),
            after,
        })
    }
}

/// Whether `next` follows `prev` closely enough to be the camera's next
/// frame of one bracket or burst.
fn camera_close(prev: &Frame, next: &Frame) -> bool {
    prev.camera == next.camera
        && ((next.time - prev.time) as f64) <= prev.exposure.unwrap_or(0.0).ceil() + CAMERA_GAP_SECS
}

fn same_bias(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < 0.05,
        (None, None) => true,
        _ => false,
    }
}

/// Groups `frames` (in capture order): camera brackets and bursts first,
/// then the rest by time (`Library::auto_stack_plan`).
fn auto_groups(frames: &[Frame], options: AutoStackOptions) -> Vec<AutoGroup> {
    let mut taken = vec![false; frames.len()];
    let mut groups: Vec<(usize, AutoGroup)> = Vec::new();
    if options.use_camera_data {
        // Brackets.
        let mut i = 0;
        while i < frames.len() {
            if frames[i].mode != Some(2) {
                i += 1;
                continue;
            }
            let mut run = vec![i];
            let mut j = i + 1;
            while j < frames.len()
                && frames[j].mode == Some(2)
                && camera_close(&frames[j - 1], &frames[j])
                && !run
                    .iter()
                    .any(|&k| same_bias(frames[k].bias, frames[j].bias))
            {
                run.push(j);
                j += 1;
            }
            if run.len() >= 2 {
                let top = run
                    .iter()
                    .copied()
                    .min_by(|&a, &b| {
                        let key = |k: usize| frames[k].bias.map_or(f64::MAX, f64::abs);
                        key(a).total_cmp(&key(b)).then(a.cmp(&b))
                    })
                    .unwrap_or(i);
                let mut assets = vec![frames[top].id.clone()];
                assets.extend(
                    run.iter()
                        .filter(|&&k| k != top)
                        .map(|&k| frames[k].id.clone()),
                );
                for &k in &run {
                    taken[k] = true;
                }
                groups.push((
                    i,
                    AutoGroup {
                        kind: AutoKind::Bracket,
                        assets,
                    },
                ));
            }
            i = j;
        }
        // Bursts.
        let mut i = 0;
        while i < frames.len() {
            if taken[i] || frames[i].sequence.is_none() {
                i += 1;
                continue;
            }
            let mut run = vec![i];
            let mut j = i + 1;
            while j < frames.len()
                && !taken[j]
                && frames[j].sequence.is_some()
                && frames[j].sequence == frames[j - 1].sequence.map(|n| n + 1)
                && camera_close(&frames[j - 1], &frames[j])
            {
                run.push(j);
                j += 1;
            }
            if run.len() >= 2 {
                for &k in &run {
                    taken[k] = true;
                }
                let assets = run.iter().map(|&k| frames[k].id.clone()).collect();
                groups.push((
                    i,
                    AutoGroup {
                        kind: AutoKind::Burst,
                        assets,
                    },
                ));
            }
            i = j;
        }
    }
    // The rest by capture time.
    let interval = i64::from(options.interval_secs);
    let mut run: Vec<usize> = Vec::new();
    let flush = |run: &mut Vec<usize>, groups: &mut Vec<(usize, AutoGroup)>| {
        if run.len() >= 2 {
            let assets = run.iter().map(|&k| frames[k].id.clone()).collect();
            groups.push((
                run[0],
                AutoGroup {
                    kind: AutoKind::Time,
                    assets,
                },
            ));
        }
        run.clear();
    };
    for (k, frame) in frames.iter().enumerate() {
        if taken[k] {
            continue;
        }
        if let Some(&last) = run.last()
            && frame.time - frames[last].time > interval
        {
            flush(&mut run, &mut groups);
        }
        run.push(k);
    }
    flush(&mut run, &mut groups);
    groups.sort_by_key(|(first, _)| *first);
    groups.into_iter().map(|(_, g)| g).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        id: &str,
        time: i64,
        mode: Option<u8>,
        bias: Option<f64>,
        sequence: Option<u32>,
    ) -> Frame {
        Frame {
            id: id.into(),
            time,
            camera: "Maker Body".into(),
            exposure: Some(0.01),
            mode,
            bias,
            sequence,
        }
    }

    fn opts(interval_secs: u32, use_camera_data: bool) -> AutoStackOptions {
        AutoStackOptions {
            interval_secs,
            use_camera_data,
        }
    }

    #[test]
    fn consecutive_brackets_split_on_a_repeated_bias_with_the_normal_frame_on_top() {
        let frames = [
            frame("a1", 0, Some(2), Some(0.0), None),
            frame("a2", 0, Some(2), Some(-2.0), None),
            frame("a3", 1, Some(2), Some(2.0), None),
            frame("b1", 1, Some(2), Some(-2.0), None),
            frame("b2", 2, Some(2), Some(0.0), None),
            frame("b3", 2, Some(2), Some(2.0), None),
            frame("c", 3, Some(0), Some(0.0), None),
        ];
        let groups = auto_groups(&frames, opts(60, true));
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].kind, AutoKind::Bracket);
        assert_eq!(groups[0].assets, ["a1", "a2", "a3"]);
        assert_eq!(groups[1].assets, ["b2", "b1", "b3"]);
        // Without camera data, all seven are one group by time.
        let groups = auto_groups(&frames, opts(60, false));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].assets.len(), 7);
    }

    #[test]
    fn bursts_follow_the_makers_frame_numbers_and_the_rest_the_time_gap() {
        let frames = [
            frame("s1", 0, None, None, Some(1)),
            frame("s2", 0, None, None, Some(2)),
            frame("s3", 0, None, None, Some(3)),
            frame("t1", 1, None, None, Some(1)),
            frame("t2", 1, None, None, Some(2)),
            frame("x", 30, None, None, None),
            frame("y", 35, None, None, None),
            frame("z", 100, None, None, None),
        ];
        let groups = auto_groups(&frames, opts(10, true));
        let kinds: Vec<_> = groups.iter().map(|g| (g.kind, g.assets.clone())).collect();
        assert_eq!(
            kinds,
            [
                (
                    AutoKind::Burst,
                    vec!["s1".to_string(), "s2".into(), "s3".into()]
                ),
                (AutoKind::Burst, vec!["t1".to_string(), "t2".into()]),
                (AutoKind::Time, vec!["x".to_string(), "y".into()]),
            ]
        );
    }

    #[test]
    fn a_long_pause_ends_a_bracket() {
        let frames = [
            frame("a", 0, Some(2), Some(0.0), None),
            frame("b", 60, Some(2), Some(-1.0), None),
        ];
        assert!(auto_groups(&frames, opts(1, true)).is_empty());
    }
}
