// SPDX-License-Identifier: AGPL-3.0-or-later
//! One app-wide memory budget for heavy work (G74, BRK1; design: notes
//! benchmark-results/memb1-2026-11-26/REPORT.md §4).
//!
//! On a 14 GB laptop whose integrated GPU shares the system memory, each of
//! seven admission rules read the memory *now* and decided alone: the step
//! prefetch saw a non-zero hold budget, a Subject run asked for (a click, or
//! a pending Subject layer computed as a photo opens) saw no check at all,
//! the AI work made ahead saw too little — and the allocations followed the
//! decisions by 1–6 s, so none saw what the others were about to take. The
//! sum hit the watchdog (WMEM21: 9 of 9 Subject clicks 10–20 s after Develop
//! opened).
//!
//! Here every heavy consumer *reserves* its estimated peak before it
//! allocates and holds the [`Reservation`] while it runs. The room a new
//! request sees is
//!
//! `available now − floor − Σ unspent part of the reservations held`,
//!
//! where a reservation's unspent part is its estimate minus what the app has
//! grown by since it started (what it allocated is already out of
//! "available"). The **floor** is max(1.5 GB, 10 % of the physical memory)
//! available: the watchdog's bar.
//!
//! Classes, strict priority:
//! - [`Class::Visible`] (P0): the open photo. Never refused; when short, it
//!   cancels speculative work and reclaims. Its frames are recorded with
//!   [`Broker::hold`] (no decision) so what they will allocate counts.
//! - [`Class::Asked`] (P1): what the photographer asked for — a click on
//!   Subject/Sky, opening the masking tools, and the pending AI layers of
//!   the open photo (render correctness). One exclusive P1 at a time. When
//!   the room misses its estimate it waits briefly for the visible photo's
//!   decode and frames (bounded), then cancels speculative work under way
//!   and waits for it to stop (bounded: `CANCEL_WAIT`; what it holds counts
//!   until it is released), reclaims caches from the
//!   [`ledger`](Broker::register_reclaim) (only buffers nothing else holds),
//!   reads the memory again, and if the room still misses the full estimate
//!   runs its *reduced plan* ([`Plan::Reduced`]) rather than wait. A request
//!   that starts only when it fits ([`Request::only_when_it_fits`]: the AI
//!   mask runs) does not start beyond the room at all: it waits
//!   (`Wait::Memory`) until a plan fits (G74/BRK2).
//! - [`Class::Export`] (P2): exports and batch AI (step 2 wires them; the
//!   rule is here): a new unit does not start while a P1 waits or runs, nor
//!   if it would leave less than the click reserve.
//! - [`Class::Speculative`] (P3): step prefetch, multi-view decodes, AI masks
//!   and model loads made ahead, the model fetch. Start only when the room
//!   covers their estimate plus the **click reserve** (what a click on
//!   Subject would need on this machine), not while the visible photo works
//!   or a P1 waits; the first to yield (cancellable).
//!
//! Estimates come from the callers (measured numbers, documented there); a
//! reservation that ran alone records its real peak growth, and the next
//! request with the same label uses max(estimate, last measured).
//!
//! Pure logic: the readings, the visible photo's state and the release
//! callbacks come from the caller (the app's `memory_broker`).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MB: u64 = 1_000_000;
pub const GB: u64 = 1_000_000_000;

/// The least memory left available: the Lenovo recheck's watchdog bar
/// (WMEM18–21 stop the app below 1.5 GB available).
pub const FLOOR_MIN: u64 = 1_500_000_000;

/// The memory a request never takes from the system: max(1.5 GB, 10 % of
/// the physical memory `total`).
pub fn floor(total: u64) -> u64 {
    (total / 10).max(FLOOR_MIN)
}

/// Share of the physical memory the app's own memory stays within for
/// speculative work: as `memory_pressure::WORK_SHARE` (G76/WG74: the multi
/// view with seven 39–61 MP RAWs at 100 % reached 10.4 GB of 14 GB).
pub const SPECULATIVE_SHARE: f64 = 0.45;

/// How long an asked request (P1) waits at most for the visible photo's
/// decode and frames when the room misses its estimate (MEM20: a click's run
/// beside the new layer's full-resolution frame took a 14 GB laptop to
/// 9.6 GB; the waits measured 1.6–1.8 s).
pub const ASKED_WAITS_FOR_VISIBLE: Duration = Duration::from_secs(5);

/// The same for a pending layer computed as the photo opens: nobody waits on
/// a click, and the photo's first frames are under way then.
pub const PENDING_WAITS_FOR_VISIBLE: Duration = Duration::from_secs(15);

/// How long a P0/P1 request whose plan does not fit beside speculative work
/// it cancelled waits at most for that work to stop: an ONNX Runtime run
/// stops at once (`RunOptions::terminate`), a decode at its next cancel
/// check, a model load only when it ends — Subject's CPU session from the
/// saved optimized graph 4.5–7.3 s, 14.6 s once on a 14 GB laptop
/// (WMEM21). Its reservation counts meanwhile; after this the request runs
/// with what fits (never refused).
pub const CANCEL_WAIT: Duration = Duration::from_secs(20);

/// Priority classes, highest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    Visible,
    Asked,
    Export,
    Speculative,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Visible => "P0 visible",
            Class::Asked => "P1 asked",
            Class::Export => "P2 export",
            Class::Speculative => "P3 speculative",
        }
    }
}

/// The memory now: physical `total` (0: unknown), what the system has
/// `available`, the app's own (`app`, None when unknown).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub total: u64,
    pub available: u64,
    pub app: Option<u64>,
}

/// What a consumer asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub class: Class,
    /// For the log and the measured-peak correction.
    pub label: &'static str,
    /// Estimated peak growth of the app with the full plan.
    pub need: u64,
    /// Estimated peak growth with the reduced plan, when the work has one.
    pub reduced: Option<u64>,
    /// At most one exclusive request of a class runs at once (P1: one heavy
    /// run; P3: one AI job made ahead), and an exclusive P3 waits for an
    /// exclusive P1 too.
    pub exclusive: bool,
    /// What a P2/P3 request must leave beside its estimate (the click
    /// reserve, `click_reserve`).
    pub reserve: u64,
    /// How long a P1 waits for the visible photo at most.
    pub visible_wait: Duration,
    /// A P1 that starts only when one of its plans fits the room (after the
    /// visible wait, cancelling speculative work and reclaiming); until then
    /// it waits (`Wait::Memory`). Without it a P1 is never refused (G74/BRK2:
    /// a Subject run granted beyond the room took a 14 GB laptop past the
    /// watchdog in 16 of 16 clicks, WBRK1).
    pub fits_only: bool,
    /// Speculative work an asked request may be waiting for (the low-peak
    /// Subject model's fetch): it is not held back by a P1 that waits, and
    /// leaves no click reserve then (G74/BRK2 review: a click waiting for
    /// memory blocked the download that would have let it fit).
    pub serves_asked: bool,
}

impl Request {
    pub fn new(class: Class, label: &'static str, need: u64) -> Self {
        Self {
            class,
            label,
            need,
            reduced: None,
            exclusive: false,
            reserve: 0,
            visible_wait: ASKED_WAITS_FOR_VISIBLE,
            fits_only: false,
            serves_asked: false,
        }
    }

    pub fn serves_asked(mut self) -> Self {
        self.serves_asked = true;
        self
    }

    pub fn only_when_it_fits(mut self) -> Self {
        self.fits_only = true;
        self
    }

    pub fn reduced(mut self, reduced: u64) -> Self {
        self.reduced = Some(reduced);
        self
    }

    pub fn exclusive(mut self) -> Self {
        self.exclusive = true;
        self
    }

    pub fn reserve(mut self, reserve: u64) -> Self {
        self.reserve = reserve;
        self
    }

    pub fn visible_wait(mut self, wait: Duration) -> Self {
        self.visible_wait = wait;
        self
    }
}

/// The plan a granted request runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Full,
    /// The request's reduced plan (`Request::reduced`): the full one does not
    /// fit even after cancelling speculative work and reclaiming.
    Reduced,
}

/// Why a request does not start yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Another exclusive request runs (`Request::exclusive`).
    Exclusive,
    /// The visible photo's decode or a frame of it is under way.
    Visible,
    /// A request the photographer made waits or runs (P2/P3 do not start).
    Asked,
    /// Speculative work cancelled for this request has not stopped yet.
    Stopping,
    /// The room is below the request's need (estimate + reserve).
    Memory { room: u64, need: u64 },
}

/// The broker's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Start with `plan` after reclaiming `reclaim` bytes from the ledger.
    Grant {
        plan: Plan,
        reclaim: u64,
    },
    /// Cancel these speculative reservations (their ids) first, then ask
    /// again (`Wait::Stopping` until they are released or `CANCEL_WAIT`).
    Cancel(Vec<u64>),
    Wait(Wait),
}

/// A reservation held, as `decide` sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    pub id: u64,
    pub class: Class,
    pub estimate: u64,
    /// The app's memory when it was granted (None: unknown).
    pub app_at_start: Option<u64>,
    pub exclusive: bool,
    /// Speculative work that stops when asked (`Broker::try_reserve`'s
    /// `cancel`).
    pub cancellable: bool,
    /// How long ago it was cancelled (None: not cancelled). It counts until
    /// it is released: it may still be allocating.
    pub cancelled_for: Option<Duration>,
}

/// The unspent part of the reservations `held` with the app at `app_now`:
/// Σ estimates minus what the app grew by since they started. Growth is
/// shared between reservations that run at once, so it counts once: at
/// most the growth since the oldest one started, and per reservation at
/// most its own estimate and the growth since its start. Unknown app memory:
/// nothing counts as spent.
pub fn unspent(held: &[Held], app_now: Option<u64>) -> u64 {
    let total: u64 = held.iter().map(|h| h.estimate).sum();
    let Some(now) = app_now else {
        return total;
    };
    let growth = |h: &Held| h.app_at_start.map_or(0, |start| now.saturating_sub(start));
    let per: u64 = held.iter().map(|h| h.estimate.min(growth(h))).sum();
    let since_oldest = held.iter().map(growth).max().unwrap_or(0);
    total.saturating_sub(per.min(since_oldest))
}

/// What `decide` reads besides the request.
#[derive(Clone, Copy, Debug)]
pub struct Situation<'a> {
    pub held: &'a [Held],
    pub reading: Reading,
    /// The visible photo's decode or a frame of it is under way.
    pub visible_busy: bool,
    /// How long this request has waited already.
    pub waited: Duration,
    /// A P1 request waits for its start (another than this one).
    pub asked_waiting: bool,
    /// Bytes the reclaim ledger could release now.
    pub reclaimable: u64,
}

/// The room for new work: available − floor − unspent reservations.
pub fn room(situation: &Situation) -> u64 {
    let r = situation.reading;
    r.available
        .saturating_sub(floor(r.total))
        .saturating_sub(unspent(situation.held, r.app))
}

/// The room for speculative work: `room`, and the app within
/// `SPECULATIVE_SHARE` of the physical memory.
fn speculative_room(situation: &Situation) -> u64 {
    let room = room(situation);
    let r = situation.reading;
    match r.app {
        Some(app) => room.min(
            ((r.total as f64 * SPECULATIVE_SHARE) as u64)
                .saturating_sub(app)
                .saturating_sub(unspent(situation.held, r.app)),
        ),
        None => room,
    }
}

/// The broker's rule for `request` in `situation` (module docs).
pub fn decide(request: &Request, situation: &Situation) -> Decision {
    let grant = |plan, reclaim| Decision::Grant { plan, reclaim };
    // Memory not readable: as the admissions before (everything starts).
    if situation.reading.total == 0 {
        return grant(Plan::Full, 0);
    }
    let exclusive_held = |at_most: Class| {
        situation
            .held
            .iter()
            .any(|h| h.exclusive && h.class <= at_most)
    };
    let room = room(situation);
    match request.class {
        Class::Visible | Class::Asked => {
            if request.class == Class::Asked && request.exclusive && exclusive_held(Class::Asked) {
                return Decision::Wait(Wait::Exclusive);
            }
            if room >= request.need {
                return grant(Plan::Full, 0);
            }
            if request.class == Class::Asked
                && situation.visible_busy
                && situation.waited < request.visible_wait
            {
                return Decision::Wait(Wait::Visible);
            }
            // Speculative work under way stops first; it counts until it
            // is released (it may still be allocating), up to CANCEL_WAIT.
            let to_cancel: Vec<u64> = situation
                .held
                .iter()
                .filter(|h| {
                    h.class == Class::Speculative && h.cancellable && h.cancelled_for.is_none()
                })
                .map(|h| h.id)
                .collect();
            if !to_cancel.is_empty() {
                return Decision::Cancel(to_cancel);
            }
            if situation.held.iter().any(|h| {
                h.class == Class::Speculative && h.cancelled_for.is_some_and(|d| d < CANCEL_WAIT)
            }) {
                return Decision::Wait(Wait::Stopping);
            }
            let short = request.need - room;
            if situation.reclaimable >= short {
                return grant(Plan::Full, short);
            }
            if let Some(reduced) = request.reduced {
                let short = reduced.saturating_sub(room);
                if situation.reclaimable >= short || !request.fits_only {
                    return grant(Plan::Reduced, short.min(situation.reclaimable));
                }
            }
            if request.fits_only {
                // Not started: waits until one of its plans fits.
                let need = request.reduced.unwrap_or(request.need).min(request.need);
                return Decision::Wait(Wait::Memory {
                    room: room.saturating_add(situation.reclaimable),
                    need,
                });
            }
            // No reduced plan: it runs as it is (never refused), with
            // everything the ledger can give.
            grant(Plan::Full, situation.reclaimable)
        }
        Class::Export => {
            if situation.asked_waiting || exclusive_held(Class::Asked) {
                return Decision::Wait(Wait::Asked);
            }
            let need = request.need.saturating_add(request.reserve);
            if room < need {
                return Decision::Wait(Wait::Memory { room, need });
            }
            grant(Plan::Full, 0)
        }
        Class::Speculative => {
            let asked_runs = situation
                .held
                .iter()
                .any(|h| h.class == Class::Asked && h.exclusive);
            let beside_waiting = request.serves_asked && situation.asked_waiting;
            if asked_runs || (situation.asked_waiting && !beside_waiting) {
                return Decision::Wait(Wait::Asked);
            }
            if request.exclusive && exclusive_held(Class::Speculative) {
                return Decision::Wait(Wait::Exclusive);
            }
            if situation.visible_busy {
                return Decision::Wait(Wait::Visible);
            }
            // Beside a waiting click it is that click's prerequisite: the
            // click's own room, without the app's speculative share or the
            // click reserve.
            let room = if beside_waiting {
                room
            } else {
                speculative_room(situation)
            };
            let reserve = if beside_waiting { 0 } else { request.reserve };
            let need = request.need.saturating_add(reserve);
            if room < need {
                return Decision::Wait(Wait::Memory { room, need });
            }
            grant(Plan::Full, 0)
        }
    }
}

/// The reclaim ledger's entries to release, lowest `order` first, for
/// `short` bytes: `entries` are (order, bytes held now); the indices taken,
/// until their bytes cover `short` (entries holding nothing are skipped).
pub fn reclaim_plan(entries: &[(u8, u64)], short: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| entries[i].0);
    let mut taken = Vec::new();
    let mut freed = 0u64;
    for i in order {
        if freed >= short {
            break;
        }
        if entries[i].1 == 0 {
            continue;
        }
        freed = freed.saturating_add(entries[i].1);
        taken.push(i);
    }
    taken
}

type Callback = Arc<dyn Fn() + Send + Sync>;
type Bytes = Arc<dyn Fn() -> u64 + Send + Sync>;

struct Entry {
    held: Held,
    label: &'static str,
    plan: Plan,
    cancel: Option<Callback>,
    /// When a P0/P1 request cancelled it.
    cancelled_at: Option<Instant>,
    /// Largest growth of the app seen since its start (`Broker::observe`).
    peak_growth: u64,
    /// No other reservation was held at any time while it ran.
    alone: bool,
}

impl Entry {
    /// As `decide` sees it now.
    fn snapshot(&self) -> Held {
        Held {
            cancelled_for: self.cancelled_at.map(|at| at.elapsed()),
            ..self.held
        }
    }
}

struct LedgerEntry {
    name: &'static str,
    order: u8,
    bytes: Bytes,
    release: Callback,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    held: Vec<Entry>,
    /// P1 requests waiting for their start (`Broker::waiting_asked`).
    asked_waiting: u32,
    /// Last measured peak growth per (label, plan).
    measured: Vec<(&'static str, Plan, u64)>,
    ledger: Vec<LedgerEntry>,
}

/// The broker: reservations held, the reclaim ledger and measured peaks.
pub struct Broker {
    inner: Mutex<Option<Inner>>,
}

/// A granted request: its memory counts as reserved until dropped.
#[must_use = "the reservation is released when dropped"]
pub struct Reservation {
    broker: &'static Broker,
    id: u64,
    plan: Plan,
    estimate: u64,
    label: &'static str,
    class: Class,
    started: Instant,
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("plan", &self.plan)
            .field("estimate", &self.estimate)
            .finish()
    }
}

/// What a release reports (for the log): how long it was held, its estimate
/// and the peak growth seen, and whether it ran alone (only then the peak is
/// its own and kept for the next estimate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Released {
    pub label: &'static str,
    pub class: Class,
    pub plan: Plan,
    pub held_for: Duration,
    pub estimate: u64,
    pub peak_growth: u64,
    pub alone: bool,
}

/// A granted request with what was done for it.
#[derive(Debug)]
pub struct Granted {
    pub reservation: Reservation,
    /// Ledger entries released for it, with the bytes they held.
    pub reclaimed: Vec<(&'static str, u64)>,
    /// The need it was decided on (with the measured-peak correction).
    pub need: u64,
    /// The room it saw.
    pub room: u64,
}

impl Reservation {
    pub fn plan(&self) -> Plan {
        self.plan
    }

    pub fn estimate(&self) -> u64 {
        self.estimate
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    pub fn class(&self) -> Class {
        self.class
    }

    pub fn held_for(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Called with what a reservation came to when it is released.
pub type ReleaseLog = fn(&Released);

impl Drop for Reservation {
    fn drop(&mut self) {
        if let Some(released) = self.broker.release(self.id, self.started) {
            (self.broker.on_release())(&released);
        }
    }
}

/// Counts a P1 request as waiting while it lives (`Broker::waiting_asked`).
pub struct AskedWaiting {
    broker: &'static Broker,
}

impl Drop for AskedWaiting {
    fn drop(&mut self) {
        self.broker.with(|inner| {
            inner.asked_waiting = inner.asked_waiting.saturating_sub(1);
        });
    }
}

static RELEASE_LOG: Mutex<Option<ReleaseLog>> = Mutex::new(None);

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

impl Broker {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        f(guard.get_or_insert_with(Inner::default))
    }

    /// Where releases are reported (the app logs them); process-wide.
    pub fn set_release_log(log: ReleaseLog) {
        *RELEASE_LOG.lock().unwrap_or_else(|e| e.into_inner()) = Some(log);
    }

    fn on_release(&self) -> ReleaseLog {
        RELEASE_LOG
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .unwrap_or(|_| {})
    }

    /// Adds a reclaim ledger entry: memory the app holds that can be let go
    /// when P0/P1 need room, released lowest `order` first. `bytes` tells
    /// what it holds now, `release` lets it go.
    pub fn register_reclaim(
        &self,
        name: &'static str,
        order: u8,
        bytes: impl Fn() -> u64 + Send + Sync + 'static,
        release: impl Fn() + Send + Sync + 'static,
    ) {
        self.with(|inner| {
            inner.ledger.retain(|e| e.name != name);
            inner.ledger.push(LedgerEntry {
                name,
                order,
                bytes: Arc::new(bytes),
                release: Arc::new(release),
            });
        });
    }

    /// A P1 request counts as waiting while the returned guard lives: P2/P3
    /// requests do not start meanwhile.
    pub fn waiting_asked(&'static self) -> AskedWaiting {
        self.with(|inner| inner.asked_waiting += 1);
        AskedWaiting { broker: self }
    }

    /// Whether any reservation is held (the app samples its memory then).
    pub fn holding(&self) -> bool {
        self.with(|inner| !inner.held.is_empty())
    }

    /// Notes the app's memory now for the peaks of the reservations held.
    pub fn observe(&self, app_now: u64) {
        self.with(|inner| {
            for entry in &mut inner.held {
                if let Some(start) = entry.held.app_at_start {
                    entry.peak_growth = entry.peak_growth.max(app_now.saturating_sub(start));
                }
            }
        });
    }

    /// The reservations held, as `decide` sees them.
    pub fn held(&self) -> Vec<Held> {
        self.with(|inner| inner.held.iter().map(Entry::snapshot).collect())
    }

    /// Whether a request of the photographer (an exclusive P1) runs or
    /// waits: background work that is not reserved here (a 1:1 preview's
    /// write) holds back meanwhile.
    pub fn asked_active(&self) -> bool {
        self.with(|inner| {
            inner.asked_waiting > 0
                || inner
                    .held
                    .iter()
                    .any(|e| e.held.class == Class::Asked && e.held.exclusive)
        })
    }

    /// The estimate for `label` with `plan`: `estimate`, or the last peak
    /// measured for it when larger.
    pub fn corrected(&self, label: &'static str, plan: Plan, estimate: u64) -> u64 {
        self.with(|inner| {
            inner
                .measured
                .iter()
                .find(|(l, p, _)| *l == label && *p == plan)
                .map_or(estimate, |(_, _, measured)| estimate.max(*measured))
        })
    }

    /// Decides `request` with the memory `read` gives now (`decide`): when
    /// it says to cancel speculative work, cancels it and answers
    /// `Wait::Stopping` (ask again); when granted, releases what the
    /// decision says from the ledger, reads the memory again and, if the
    /// full plan's room is still not there, takes the reduced plan; records
    /// the reservation. `waited`: how long it has waited; `cancel`: how to
    /// stop it, for speculative work (P0/P1 call it when they need its room).
    pub fn try_reserve(
        &'static self,
        request: Request,
        read: &dyn Fn() -> Reading,
        visible_busy: bool,
        waited: Duration,
        cancel: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Granted, Wait> {
        let mut request = request;
        request.need = self.corrected(request.label, Plan::Full, request.need);
        if let Some(reduced) = request.reduced {
            request.reduced = Some(self.corrected(request.label, Plan::Reduced, reduced));
        }
        let reading = read();
        let decided = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let inner = guard.get_or_insert_with(Inner::default);
            let held: Vec<Held> = inner.held.iter().map(Entry::snapshot).collect();
            let ledger_bytes: Vec<(u8, u64)> = inner
                .ledger
                .iter()
                .map(|e| (e.order, (e.bytes)()))
                .collect();
            let situation = Situation {
                held: &held,
                reading,
                visible_busy,
                waited,
                asked_waiting: inner.asked_waiting > u32::from(request.class == Class::Asked),
                reclaimable: ledger_bytes.iter().map(|(_, b)| b).sum(),
            };
            let room = room(&situation);
            match decide(&request, &situation) {
                Decision::Wait(wait) => return Err(wait),
                Decision::Cancel(ids) => {
                    let now = Instant::now();
                    let cancels: Vec<Callback> = inner
                        .held
                        .iter_mut()
                        .filter(|e| ids.contains(&e.held.id))
                        .filter_map(|e| {
                            e.cancelled_at = Some(now);
                            e.cancel.clone()
                        })
                        .collect();
                    Err(cancels)
                }
                Decision::Grant { plan, reclaim } => {
                    let releases: Vec<(&'static str, u64, Callback)> = if reclaim > 0 {
                        reclaim_plan(&ledger_bytes, reclaim)
                            .into_iter()
                            .map(|i| {
                                let e = &inner.ledger[i];
                                (e.name, ledger_bytes[i].1, e.release.clone())
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let estimate = match plan {
                        Plan::Full => request.need,
                        Plan::Reduced => request.reduced.unwrap_or(request.need),
                    };
                    inner.next_id += 1;
                    let id = inner.next_id;
                    let alone = inner.held.is_empty();
                    for other in &mut inner.held {
                        other.alone = false;
                    }
                    inner.held.push(Entry {
                        held: Held {
                            id,
                            class: request.class,
                            estimate,
                            app_at_start: reading.app,
                            exclusive: request.exclusive,
                            cancellable: cancel.is_some(),
                            cancelled_for: None,
                        },
                        label: request.label,
                        plan,
                        cancel,
                        cancelled_at: None,
                        peak_growth: 0,
                        alone,
                    });
                    let reservation = Reservation {
                        broker: self,
                        id,
                        plan,
                        estimate,
                        label: request.label,
                        class: request.class,
                        started: Instant::now(),
                    };
                    Ok((reservation, room, releases))
                }
            }
        };
        // Outside the broker's lock: the callbacks take other locks.
        let (mut reservation, room, releases) = match decided {
            Ok(granted) => granted,
            Err(cancels) => {
                for cancel in cancels {
                    cancel();
                }
                return Err(Wait::Stopping);
            }
        };
        let mut reclaimed = Vec::new();
        for (name, bytes, release) in releases {
            release();
            reclaimed.push((name, bytes));
        }
        // What the ledger promised may not all come back (a buffer another
        // holder took meanwhile): the full plan only if its room is there,
        // and a request that starts only when it fits not at all if even its
        // reduced plan's room is not.
        let now = if reclaimed.is_empty() {
            None
        } else {
            Some(read()).filter(|now| now.total > 0)
        };
        if let Some(now) = now {
            let others: Vec<Held> = self.with(|inner| {
                inner
                    .held
                    .iter()
                    .filter(|e| e.held.id != reservation.id)
                    .map(Entry::snapshot)
                    .collect()
            });
            let room_now = now
                .available
                .saturating_sub(floor(now.total))
                .saturating_sub(unspent(&others, now.app));
            if reservation.plan == Plan::Full
                && room_now < request.need
                && let Some(reduced) = request.reduced
            {
                self.with(|inner| {
                    if let Some(e) = inner.held.iter_mut().find(|e| e.held.id == reservation.id) {
                        e.plan = Plan::Reduced;
                        e.held.estimate = reduced;
                    }
                });
                reservation.plan = Plan::Reduced;
                reservation.estimate = reduced;
            }
            if request.fits_only && room_now < reservation.estimate {
                let need = reservation.estimate;
                self.withdraw(reservation);
                return Err(Wait::Memory {
                    room: room_now,
                    need,
                });
            }
        }
        Ok(Granted {
            reservation,
            reclaimed,
            need: request.need,
            room,
        })
    }

    /// Records `estimate` as held for visible work (P0: the open photo's
    /// frame) without deciding anything: it never waits, and what it will
    /// allocate counts for the requests decided while it runs (G74/BRK2: the
    /// new layer's full-resolution frame a Subject click starts grew a 14 GB
    /// laptop by 1.9 GB beside the run, unseen by the broker, WBRK1).
    pub fn hold(
        &'static self,
        class: Class,
        label: &'static str,
        estimate: u64,
        app_now: Option<u64>,
    ) -> Reservation {
        self.with(|inner| {
            inner.next_id += 1;
            let id = inner.next_id;
            let alone = inner.held.is_empty();
            for other in &mut inner.held {
                other.alone = false;
            }
            inner.held.push(Entry {
                held: Held {
                    id,
                    class,
                    estimate,
                    app_at_start: app_now,
                    exclusive: false,
                    cancellable: false,
                    cancelled_for: None,
                },
                label,
                plan: Plan::Full,
                cancel: None,
                cancelled_at: None,
                peak_growth: 0,
                alone,
            });
            Reservation {
                broker: self,
                id,
                plan: Plan::Full,
                estimate,
                label,
                class,
                started: Instant::now(),
            }
        })
    }

    /// Takes back a grant that is not started after all, without a release
    /// report or a measured peak.
    fn withdraw(&self, reservation: Reservation) {
        let id = reservation.id;
        std::mem::forget(reservation);
        self.with(|inner| inner.held.retain(|e| e.held.id != id));
    }

    fn release(&self, id: u64, started: Instant) -> Option<Released> {
        self.with(|inner| {
            let at = inner.held.iter().position(|e| e.held.id == id)?;
            let entry = inner.held.remove(at);
            if entry.alone && entry.held.app_at_start.is_some() && entry.peak_growth > 0 {
                inner
                    .measured
                    .retain(|(l, p, _)| !(*l == entry.label && *p == entry.plan));
                inner
                    .measured
                    .push((entry.label, entry.plan, entry.peak_growth));
            }
            Some(Released {
                label: entry.label,
                class: entry.held.class,
                plan: entry.plan,
                held_for: started.elapsed(),
                estimate: entry.held.estimate,
                peak_growth: entry.peak_growth,
                alone: entry.alone,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    /// The Lenovo of WMEM21 (14 GB visible as 15859 MB).
    const LENOVO: u64 = 15_859 * MB;

    fn reading(available: u64, app: u64) -> Reading {
        Reading {
            total: LENOVO,
            available,
            app: Some(app),
        }
    }

    fn situation<'a>(held: &'a [Held], reading: Reading) -> Situation<'a> {
        Situation {
            held,
            reading,
            visible_busy: false,
            waited: Duration::ZERO,
            asked_waiting: false,
            reclaimable: 0,
        }
    }

    fn held(id: u64, class: Class, estimate: u64, app_at_start: u64) -> Held {
        Held {
            id,
            class,
            estimate,
            app_at_start: Some(app_at_start),
            exclusive: false,
            cancellable: false,
            cancelled_for: None,
        }
    }

    #[test]
    fn the_floor_is_the_watchdog_bar_or_a_tenth() {
        assert_eq!(floor(LENOVO), 1_585_900_000);
        assert_eq!(floor(8 * GB), FLOOR_MIN);
        assert_eq!(floor(24 * GB), 2_400_000_000);
    }

    #[test]
    fn a_reservation_counts_only_what_it_has_not_taken_yet() {
        let h = [held(1, Class::Speculative, 2 * GB, 3 * GB)];
        assert_eq!(unspent(&h, Some(3 * GB)), 2 * GB);
        assert_eq!(unspent(&h, Some(3 * GB + 500 * MB)), 1500 * MB);
        assert_eq!(unspent(&h, Some(6 * GB)), 0);
        // The app shrank meanwhile (a cache let go): nothing spent.
        assert_eq!(unspent(&h, Some(2 * GB)), 2 * GB);
        // Memory unknown: all of it.
        assert_eq!(unspent(&h, None), 2 * GB);
    }

    #[test]
    fn growth_shared_by_two_reservations_counts_once() {
        // Both started at 3 GB, the app grew by 1 GB: 1 GB spent, not 2.
        let h = [
            held(1, Class::Speculative, 2 * GB, 3 * GB),
            held(2, Class::Asked, 2 * GB, 3 * GB),
        ];
        assert_eq!(unspent(&h, Some(4 * GB)), 3 * GB);
    }

    #[test]
    fn speculative_work_leaves_the_click_reserve() {
        // MEMB1 §5: 4-up open on the Lenovo with a browser open, 4.9 GB
        // available at the Develop click, app 2.8 GB: a 30 MP neighbour's
        // decode (1.2 GB) plus the 3.0 GB click reserve does not fit.
        let decode = Request::new(Class::Speculative, "decode", 1200 * MB).reserve(3 * GB);
        let none: [Held; 0] = [];
        assert_eq!(
            decide(&decode, &situation(&none, reading(4900 * MB, 2800 * MB))),
            Decision::Wait(Wait::Memory {
                room: 4900 * MB - floor(LENOVO),
                need: 4200 * MB
            })
        );
        // 8.5 GB available (the batch after memory was freed): it starts.
        assert!(matches!(
            decide(&decode, &situation(&none, reading(8500 * MB, 2800 * MB))),
            Decision::Grant {
                plan: Plan::Full,
                ..
            }
        ));
    }

    #[test]
    fn speculative_work_stays_within_the_apps_share() {
        let decode = Request::new(Class::Speculative, "decode", 1200 * MB);
        let none: [Held; 0] = [];
        // Plenty available but the app at 6.5 GB of 15.9 (share 7.1 GB).
        assert!(matches!(
            decide(&decode, &situation(&none, reading(9 * GB, 6500 * MB))),
            Decision::Wait(Wait::Memory { .. })
        ));
    }

    #[test]
    fn speculative_work_waits_for_the_visible_photo_and_for_the_photographer() {
        let job = Request::new(Class::Speculative, "subject ahead", 4700 * MB).exclusive();
        let none: [Held; 0] = [];
        let mut s = situation(&none, reading(12 * GB, 2 * GB));
        s.visible_busy = true;
        assert_eq!(decide(&job, &s), Decision::Wait(Wait::Visible));
        s.visible_busy = false;
        s.asked_waiting = true;
        assert_eq!(decide(&job, &s), Decision::Wait(Wait::Asked));
        let click = [Held {
            exclusive: true,
            ..held(1, Class::Asked, 2 * GB, 2 * GB)
        }];
        let s = situation(&click, reading(12 * GB, 2 * GB));
        assert_eq!(decide(&job, &s), Decision::Wait(Wait::Asked));
    }

    #[test]
    fn one_exclusive_job_made_ahead_at_a_time() {
        let sky = Request::new(Class::Speculative, "sky ahead", 1200 * MB).exclusive();
        let subject = [Held {
            exclusive: true,
            ..held(1, Class::Speculative, 3700 * MB, GB)
        }];
        assert_eq!(
            decide(&sky, &situation(&subject, reading(20 * GB, GB))),
            Decision::Wait(Wait::Exclusive)
        );
        // A non-exclusive one (the model fetch) only needs the room.
        let fetch = Request::new(Class::Speculative, "fetch", 700 * MB);
        assert!(matches!(
            decide(&fetch, &situation(&subject, reading(20 * GB, GB))),
            Decision::Grant { .. }
        ));
    }

    #[test]
    fn a_click_with_room_starts_at_once() {
        let click = Request::new(Class::Asked, "subject", 2600 * MB).exclusive();
        let none: [Held; 0] = [];
        assert_eq!(
            decide(&click, &situation(&none, reading(8 * GB, 3 * GB))),
            Decision::Grant {
                plan: Plan::Full,
                reclaim: 0
            }
        );
    }

    #[test]
    fn a_short_click_waits_for_the_visible_photo_but_not_for_long() {
        let click = Request::new(Class::Asked, "subject", 4 * GB)
            .reduced(3 * GB)
            .exclusive();
        let none: [Held; 0] = [];
        let mut s = situation(&none, reading(4 * GB, 3 * GB));
        s.visible_busy = true;
        assert_eq!(decide(&click, &s), Decision::Wait(Wait::Visible));
        s.waited = ASKED_WAITS_FOR_VISIBLE;
        assert!(matches!(decide(&click, &s), Decision::Grant { .. }));
    }

    #[test]
    fn a_click_cancels_speculative_work_and_waits_for_it_to_stop() {
        // A neighbour's decode reserved 1.2 GB and took 0.2 GB so far.
        let decode = |cancelled_for| {
            [Held {
                cancellable: true,
                cancelled_for,
                ..held(7, Class::Speculative, 1200 * MB, 3 * GB)
            }]
        };
        let click = Request::new(Class::Asked, "subject", 2600 * MB).exclusive();
        // Room beside the decode (1.0 GB of it unspent): 5.2 − 1.586 − 1.0
        // = 2.614 GB ≥ 2.6: fits without cancelling.
        let r = reading(5200 * MB, 3200 * MB);
        assert!(matches!(
            decide(&click, &situation(&decode(None), r)),
            Decision::Grant {
                plan: Plan::Full,
                reclaim: 0
            }
        ));
        // 4.8 GB available: 2.214 GB room, short; the decode is cancelled
        // first, and the click waits until it stops.
        let r = reading(4800 * MB, 3200 * MB);
        assert_eq!(
            decide(&click, &situation(&decode(None), r)),
            Decision::Cancel(vec![7])
        );
        let stopping = decode(Some(Duration::from_millis(300)));
        assert_eq!(
            decide(&click, &situation(&stopping, r)),
            Decision::Wait(Wait::Stopping)
        );
        // Released: its room is free for real.
        let none: [Held; 0] = [];
        assert!(matches!(
            decide(&click, &situation(&none, reading(4800 * MB, 3400 * MB))),
            Decision::Grant {
                plan: Plan::Full,
                ..
            }
        ));
        // Still not stopped after CANCEL_WAIT (a model load): it counts and
        // the click plans without it.
        let stuck = decode(Some(CANCEL_WAIT));
        assert!(matches!(
            decide(&click, &situation(&stuck, r)),
            Decision::Grant { .. }
        ));
    }

    #[test]
    fn a_click_reclaims_then_falls_back_to_its_reduced_plan() {
        let click = Request::new(Class::Asked, "subject", 3 * GB)
            .reduced(2 * GB)
            .exclusive();
        let none: [Held; 0] = [];
        // Room 4.0 − 1.586 = 2.414 GB: 0.586 GB short of the full plan.
        let mut s = situation(&none, reading(4 * GB, 3 * GB));
        s.reclaimable = GB;
        assert_eq!(
            decide(&click, &s),
            Decision::Grant {
                plan: Plan::Full,
                reclaim: 585_900_000
            }
        );
        // Not enough to reclaim: the reduced plan, which fits as it is.
        s.reclaimable = 100 * MB;
        assert_eq!(
            decide(&click, &s),
            Decision::Grant {
                plan: Plan::Reduced,
                reclaim: 0
            }
        );
        // Even the reduced plan is short: it still runs, reclaiming all.
        let mut s = situation(&none, reading(3 * GB, 3 * GB));
        s.reclaimable = 100 * MB;
        assert_eq!(
            decide(&click, &s),
            Decision::Grant {
                plan: Plan::Reduced,
                reclaim: 100 * MB
            }
        );
    }

    #[test]
    fn the_photographer_is_never_refused() {
        let click = Request::new(Class::Asked, "sky", 1200 * MB).exclusive();
        let none: [Held; 0] = [];
        let mut s = situation(&none, reading(0, 3 * GB));
        s.visible_busy = true;
        s.waited = Duration::from_secs(60);
        assert!(matches!(
            decide(&click, &s),
            Decision::Grant {
                plan: Plan::Full,
                ..
            }
        ));
        let visible = Request::new(Class::Visible, "decode", 4 * GB);
        assert!(matches!(
            decide(&visible, &situation(&none, reading(0, 3 * GB))),
            Decision::Grant { .. }
        ));
    }

    #[test]
    fn one_heavy_asked_run_at_a_time() {
        let running = [Held {
            exclusive: true,
            ..held(1, Class::Asked, 2 * GB, 3 * GB)
        }];
        let sky = Request::new(Class::Asked, "sky", 800 * MB).exclusive();
        assert_eq!(
            decide(&sky, &situation(&running, reading(20 * GB, 3 * GB))),
            Decision::Wait(Wait::Exclusive)
        );
    }

    #[test]
    fn exports_yield_to_the_photographer_and_leave_the_reserve() {
        let photo = Request::new(Class::Export, "export", 2 * GB).reserve(3 * GB);
        let none: [Held; 0] = [];
        let mut s = situation(&none, reading(20 * GB, 3 * GB));
        s.asked_waiting = true;
        assert_eq!(decide(&photo, &s), Decision::Wait(Wait::Asked));
        assert!(matches!(
            decide(&photo, &situation(&none, reading(6 * GB, 3 * GB))),
            Decision::Wait(Wait::Memory { .. })
        ));
        assert!(matches!(
            decide(&photo, &situation(&none, reading(7 * GB, 3 * GB))),
            Decision::Grant { .. }
        ));
    }

    #[test]
    fn unknown_memory_admits_as_before() {
        let job = Request::new(Class::Speculative, "decode", u64::MAX);
        let none: [Held; 0] = [];
        let r = Reading {
            total: 0,
            available: 0,
            app: None,
        };
        assert!(matches!(
            decide(&job, &situation(&none, r)),
            Decision::Grant { .. }
        ));
    }

    #[test]
    fn the_ledger_lets_go_lowest_order_first_until_enough() {
        // held decodes 0.9 GB, caches 0.5 GB, idle models 0.4 GB.
        let entries = [(2, 400 * MB), (0, 900 * MB), (1, 500 * MB)];
        assert_eq!(reclaim_plan(&entries, 800 * MB), vec![1]);
        assert_eq!(reclaim_plan(&entries, 1000 * MB), vec![1, 2]);
        assert_eq!(reclaim_plan(&entries, 5 * GB), vec![1, 2, 0]);
        assert!(reclaim_plan(&entries, 0).is_empty());
        // Empty entries are skipped.
        assert_eq!(reclaim_plan(&[(0, 0), (1, 300 * MB)], 100 * MB), vec![1]);
    }

    // The stateful broker, each test with its own (leaked) instance.
    fn broker() -> &'static Broker {
        Box::leak(Box::new(Broker::new()))
    }

    #[test]
    fn a_reservation_counts_until_dropped() {
        let b = broker();
        let r = reading(8 * GB, 3 * GB);
        let first = b
            .try_reserve(
                Request::new(Class::Speculative, "a", 2 * GB),
                &|| r,
                false,
                Duration::ZERO,
                None,
            )
            .expect("room for it");
        assert_eq!(b.held().len(), 1);
        // 8 − 1.586 − 2 = 4.41 GB left, but the app's share (7.14 GB) has
        // only 7.14 − 3 − 2 = 2.14 GB: a second 4 GB one waits.
        let second = Request::new(Class::Speculative, "b", 4 * GB);
        assert!(matches!(
            b.try_reserve(second, &|| r, false, Duration::ZERO, None),
            Err(Wait::Memory { .. })
        ));
        drop(first);
        assert!(b.held().is_empty());
        assert!(
            b.try_reserve(second, &|| r, false, Duration::ZERO, None)
                .is_ok()
        );
    }

    #[test]
    fn a_click_cancels_preempts_and_reclaims_through_the_broker() {
        let b = broker();
        static CANCELLED: AtomicUsize = AtomicUsize::new(0);
        static HELD_DECODES: AtomicU64 = AtomicU64::new(900 * MB);
        static AVAILABLE: AtomicU64 = AtomicU64::new(5 * GB);
        b.register_reclaim(
            "held decodes",
            0,
            || HELD_DECODES.load(Ordering::SeqCst),
            || {
                // Letting them go gives their memory back.
                AVAILABLE.fetch_add(HELD_DECODES.swap(0, Ordering::SeqCst), Ordering::SeqCst);
            },
        );
        let r = || reading(AVAILABLE.load(Ordering::SeqCst), 3 * GB);
        let prefetch = b
            .try_reserve(
                Request::new(Class::Speculative, "prefetch", 1200 * MB),
                &r,
                false,
                Duration::ZERO,
                Some(Arc::new(|| {
                    CANCELLED.fetch_add(1, Ordering::SeqCst);
                })),
            )
            .expect("room");
        let click = Request::new(Class::Asked, "subject click", 3500 * MB)
            .reduced(2 * GB)
            .exclusive();
        // Room beside the prefetch: 5 − 1.586 − 1.2 = 2.2 GB < 3.5: the
        // prefetch is cancelled, and the click waits for it to stop.
        assert_eq!(
            b.try_reserve(click, &r, false, Duration::ZERO, None)
                .unwrap_err(),
            Wait::Stopping
        );
        assert_eq!(CANCELLED.load(Ordering::SeqCst), 1);
        assert_eq!(
            b.try_reserve(click, &r, false, Duration::ZERO, None)
                .unwrap_err(),
            Wait::Stopping
        );
        assert_eq!(CANCELLED.load(Ordering::SeqCst), 1, "cancelled once");
        drop(prefetch);
        // Stopped: 3.414 GB room, 86 MB short: the held decodes go.
        let granted = b
            .try_reserve(click, &r, false, Duration::ZERO, None)
            .expect("never refused");
        assert_eq!(granted.reclaimed, vec![("held decodes", 900 * MB)]);
        assert_eq!(HELD_DECODES.load(Ordering::SeqCst), 0);
        assert_eq!(granted.reservation.plan(), Plan::Full);
        // A second click waits for the first; speculative work too.
        assert_eq!(
            b.try_reserve(
                Request::new(Class::Asked, "sky click", 800 * MB).exclusive(),
                &|| reading(20 * GB, 3 * GB),
                false,
                Duration::ZERO,
                None
            )
            .unwrap_err(),
            Wait::Exclusive
        );
        assert_eq!(
            b.try_reserve(
                Request::new(Class::Speculative, "decode", MB),
                &|| reading(20 * GB, 3 * GB),
                false,
                Duration::ZERO,
                None
            )
            .unwrap_err(),
            Wait::Asked
        );
        assert!(b.asked_active());
        drop(granted);
        assert!(!b.asked_active());
    }

    #[test]
    fn a_reclaim_that_does_not_come_back_falls_back_to_the_reduced_plan() {
        let b = broker();
        static PROMISED: AtomicU64 = AtomicU64::new(GB);
        // A cache that says it holds 1 GB, but its buffers are shared:
        // releasing it frees nothing.
        b.register_reclaim(
            "shared cache",
            1,
            || PROMISED.load(Ordering::SeqCst),
            || PROMISED.store(0, Ordering::SeqCst),
        );
        let r = || reading(4 * GB, 3 * GB);
        let granted = b
            .try_reserve(
                Request::new(Class::Asked, "subject click", 3 * GB)
                    .reduced(2 * GB)
                    .exclusive(),
                &r,
                false,
                Duration::ZERO,
                None,
            )
            .expect("never refused");
        assert_eq!(granted.reclaimed, vec![("shared cache", GB)]);
        assert_eq!(granted.reservation.plan(), Plan::Reduced);
        assert_eq!(granted.reservation.estimate(), 2 * GB);
    }

    #[test]
    fn a_waiting_click_holds_speculative_work_back() {
        let b = broker();
        let waiting = b.waiting_asked();
        let spec = Request::new(Class::Speculative, "decode", MB);
        assert_eq!(
            b.try_reserve(
                spec,
                &|| reading(20 * GB, 3 * GB),
                false,
                Duration::ZERO,
                None
            )
            .unwrap_err(),
            Wait::Asked
        );
        drop(waiting);
        assert!(
            b.try_reserve(
                spec,
                &|| reading(20 * GB, 3 * GB),
                false,
                Duration::ZERO,
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn a_peak_measured_alone_raises_the_next_estimate() {
        let b = broker();
        let r = reading(20 * GB, 3 * GB);
        let req = Request::new(Class::Speculative, "subject ahead", 2 * GB);
        let run = b
            .try_reserve(req, &|| r, false, Duration::ZERO, None)
            .unwrap();
        b.observe(3 * GB + 2600 * MB);
        b.observe(3 * GB + 1000 * MB);
        drop(run);
        assert_eq!(b.corrected("subject ahead", Plan::Full, 2 * GB), 2600 * MB);
        // Never lowered by a smaller measurement.
        assert_eq!(b.corrected("subject ahead", Plan::Full, 3 * GB), 3 * GB);
        // Two at once: neither peak is its own, nothing recorded.
        let a = b
            .try_reserve(
                Request::new(Class::Speculative, "x", MB),
                &|| r,
                false,
                Duration::ZERO,
                None,
            )
            .unwrap();
        let c = b
            .try_reserve(
                Request::new(Class::Speculative, "y", MB),
                &|| r,
                false,
                Duration::ZERO,
                None,
            )
            .unwrap();
        b.observe(9 * GB);
        drop(a);
        drop(c);
        assert_eq!(b.corrected("x", Plan::Full, MB), MB);
        assert_eq!(b.corrected("y", Plan::Full, MB), MB);
    }
    #[test]
    fn a_mask_run_that_does_not_fit_waits_instead_of_running_beyond_the_room() {
        // WBRK1 L7_c10_r1 (Lenovo, ~7 GB available at start): after the new
        // layer's frame, 2312 MB available, app 4728 MB: room 726 MB. The
        // reduced plan (warp cached: model 1000 + run 650 MB) does not fit.
        let click = Request::new(Class::Asked, "subject click", 2400 * MB)
            .reduced(1650 * MB)
            .exclusive()
            .only_when_it_fits();
        let none: [Held; 0] = [];
        let r = reading(2312 * MB, 4728 * MB);
        let room = 2312 * MB - floor(LENOVO);
        assert_eq!(
            decide(&click, &situation(&none, r)),
            Decision::Wait(Wait::Memory {
                room,
                need: 1650 * MB
            })
        );
        // The visible photo's frame first, as before.
        let mut s = situation(&none, r);
        s.visible_busy = true;
        assert_eq!(decide(&click, &s), Decision::Wait(Wait::Visible));
        // What the ledger can give counts: enough for the reduced plan, it
        // starts with it, reclaiming only what it needs.
        let mut s = situation(&none, r);
        s.reclaimable = GB;
        assert_eq!(
            decide(&click, &s),
            Decision::Grant {
                plan: Plan::Reduced,
                reclaim: 1650 * MB - room
            }
        );
        // Not enough with the ledger either: it still waits, and says how
        // much it would have.
        s.reclaimable = 100 * MB;
        assert_eq!(
            decide(&click, &s),
            Decision::Wait(Wait::Memory {
                room: room + 100 * MB,
                need: 1650 * MB
            })
        );
        // Room for it later (memory freed): it starts.
        assert!(matches!(
            decide(&click, &situation(&none, reading(3500 * MB, 4728 * MB))),
            Decision::Grant {
                plan: Plan::Reduced,
                reclaim: 0
            }
        ));
        // The same request without the rule runs as before (never refused).
        let mut old = click;
        old.fits_only = false;
        assert!(matches!(
            decide(&old, &situation(&none, r)),
            Decision::Grant {
                plan: Plan::Reduced,
                ..
            }
        ));
    }

    #[test]
    fn a_visible_frame_held_counts_for_a_click_beside_it() {
        // WBRK1 L85_c10_r1: the click came as its new layer's frame started
        // (app 3504 MB, 5651 MB available); the frame then grew the app by
        // ~1.9 GB beside the run. Held, the frame leaves room 2125 MB: the
        // full plan (3013 MB) does not start beside it.
        let b = broker();
        let r = || reading(5651 * MB, 3504 * MB);
        let frame = b.hold(Class::Visible, "visible frame", 1940 * MB, Some(3504 * MB));
        let click = Request::new(Class::Asked, "subject click", 3013 * MB)
            .reduced(2263 * MB)
            .exclusive()
            .only_when_it_fits();
        assert_eq!(
            b.try_reserve(click, &r, true, Duration::ZERO, None)
                .unwrap_err(),
            Wait::Visible
        );
        // After the visible wait, still short: waits for memory, holds
        // nothing.
        assert!(matches!(
            b.try_reserve(click, &r, false, ASKED_WAITS_FOR_VISIBLE, None)
                .unwrap_err(),
            Wait::Memory { .. }
        ));
        assert_eq!(b.held().len(), 1);
        // The frame ended, its warp cached (the click's own estimate
        // without it): model + reduced run 1650 MB fit 3751 − 1586.
        drop(frame);
        let after = || reading(3751 * MB, 5404 * MB);
        let granted = b
            .try_reserve(
                Request::new(Class::Asked, "subject click", 2400 * MB)
                    .reduced(1650 * MB)
                    .exclusive()
                    .only_when_it_fits(),
                &after,
                false,
                Duration::ZERO,
                None,
            )
            .expect("fits now");
        assert_eq!(granted.reservation.plan(), Plan::Reduced);
    }

    #[test]
    fn a_reclaim_that_does_not_come_back_withdraws_a_run_that_must_fit() {
        let b = broker();
        static PROMISED: AtomicU64 = AtomicU64::new(GB);
        b.register_reclaim(
            "shared cache",
            1,
            || PROMISED.load(Ordering::SeqCst),
            || PROMISED.store(0, Ordering::SeqCst),
        );
        // Room 3.0 − 1.586 = 1.414 GB, reduced 2 GB: 0.586 GB short, the
        // ledger promises 1 GB but frees nothing.
        let r = || reading(3 * GB, 3 * GB);
        let wait = b
            .try_reserve(
                Request::new(Class::Asked, "subject click", 3 * GB)
                    .reduced(2 * GB)
                    .exclusive()
                    .only_when_it_fits(),
                &r,
                false,
                Duration::ZERO,
                None,
            )
            .unwrap_err();
        assert_eq!(
            wait,
            Wait::Memory {
                room: 3 * GB - floor(LENOVO),
                need: 2 * GB
            }
        );
        assert!(b.held().is_empty(), "nothing held for a run not started");
        assert!(!b.asked_active());
    }
    #[test]
    fn the_model_fetch_a_waiting_click_needs_runs_beside_it() {
        let none: [Held; 0] = [];
        let fetch = Request::new(Class::Speculative, "fetch", 700 * MB)
            .reserve(3 * GB)
            .serves_asked();
        let decode = Request::new(Class::Speculative, "decode", 700 * MB).reserve(3 * GB);
        let mut s = situation(&none, reading(3 * GB, 3 * GB));
        s.asked_waiting = true;
        // Room 1.41 GB: the fetch (no reserve beside the waiting click)
        // starts, other speculative work still waits for the click.
        assert!(matches!(decide(&fetch, &s), Decision::Grant { .. }));
        assert_eq!(decide(&decode, &s), Decision::Wait(Wait::Asked));
        // Review r2: the app at 6.6 GB of 15.86 (its speculative share
        // leaves 537 MB), 4 GB available: the fetch still starts.
        let mut s = situation(&none, reading(4 * GB, 6600 * MB));
        s.asked_waiting = true;
        assert!(matches!(decide(&fetch, &s), Decision::Grant { .. }));
        // Not while an asked run holds its reservation.
        let click = [Held {
            exclusive: true,
            ..held(1, Class::Asked, GB, 3 * GB)
        }];
        assert_eq!(
            decide(&fetch, &situation(&click, reading(20 * GB, 3 * GB))),
            Decision::Wait(Wait::Asked)
        );
    }
}
