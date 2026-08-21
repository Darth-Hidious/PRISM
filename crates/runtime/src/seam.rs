// Copyright (c) 2025-2026 Mirdyne. Licensed under Mirdyne Source-Available License.
//! The composability seam: components that touch the world only through a
//! handle that records how to undo what they just did.
//!
//! This repo has ~50 hand-written `Drop` guards, each one a teardown someone
//! had to reason about by hand. The limit case is the tool-server pool, whose
//! correctness depends on *struct field declaration order* — reorder two
//! fields and teardown silently inverts. `Drop` also cannot run an async
//! inverse at all. This module is the alternative: a [`Component`] registers
//! each effect's inverse at the moment it performs the effect, and the
//! [`Runtime`] runs those inverses explicitly (never in `Drop`), in an order
//! it derives from the dependency declarations.
//!
//! Two properties are load-bearing and tested:
//!
//! - **Ordering**: a provided key is withdrawn only *after* every component
//!   depending on it has deactivated, so a consumer can still read the key
//!   throughout its own deactivation. (The pool solves this once with field
//!   order; here it holds for any component graph.)
//! - **Recovery exactness**: retiring a component runs that component's
//!   inverses and nothing else. Dependents of its keys park — they do not
//!   error, and their own state is undone by their own inverses only.
//!
//! A component whose dependency is missing **parks**: it stays inactive,
//! records what it is waiting for, and activates when the key appears. Parking
//! is deliberately loud — [`Runtime::status`] names every parked component and
//! the key it waits on, and each park is traced — because replacing silent
//! degradation with silent *stalling* would be strictly worse than either.

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use async_trait::async_trait;

/// A boxed future, so inverses and activation can be `dyn`-dispatched.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One deferred inverse. `FnOnce` because an inverse runs at most once.
type Undo = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

/// A dependency key: the name under which a provider publishes a value and a
/// consumer looks it up. Const-constructible so a component can declare its
/// needs as a `static` slice.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key(Cow<'static, str>);

impl Key {
    pub const fn new(name: &'static str) -> Self {
        Key(Cow::Borrowed(name))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A borrowed dependency. Holds an `Arc`, so a value read before a withdrawal
/// stays usable through the holder's own deactivation — the withdrawal removes
/// the key from the board, never the value out from under a reader.
pub struct Dep<T>(Arc<T>);

impl<T> std::ops::Deref for Dep<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> Clone for Dep<T> {
    fn clone(&self) -> Self {
        Dep(self.0.clone())
    }
}

/// Where a component is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiberState {
    /// Loaded, never activated (or activation failed) — waiting.
    Inactive,
    /// Was active; deactivated because a dependency withdrew. Reactivates
    /// when the key returns.
    Reloading,
    Active,
    /// Inverses are running right now. Guards against re-entrant teardown.
    Unloading,
}

/// One component's observable status — the operator's answer to "why is
/// nothing happening?". A parked component names exactly what it waits for
/// and, when known, why the key went away.
#[derive(Debug, Clone)]
pub struct FiberStatus {
    pub name: String,
    pub state: FiberState,
    /// Keys this component is waiting for (empty when active).
    pub waiting_on: Vec<Key>,
    /// The reason the withdrawer gave, per waited-on key that has one.
    pub park_reasons: Vec<String>,
    /// Set when the component can never activate because its needs are
    /// satisfiable only through a dependency cycle among loaded components.
    pub blocked_by_cycle: bool,
    /// The last activation error, if activation was attempted and failed.
    pub last_error: Option<String>,
}

impl fmt::Display for FiberStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {:?}", self.name, self.state)?;
        if !self.waiting_on.is_empty() {
            let keys: Vec<String> = self.waiting_on.iter().map(|k| k.to_string()).collect();
            write!(f, " — waiting for {}", keys.join(", "))?;
        }
        for reason in &self.park_reasons {
            write!(f, " ({reason})")?;
        }
        if self.blocked_by_cycle {
            write!(f, " [dependency cycle]")?;
        }
        if let Some(error) = &self.last_error {
            write!(f, " [last error: {error}]")?;
        }
        Ok(())
    }
}

/// What `activate` decided. `Parked` is NOT an error: it means "a value I
/// need is not there yet" — the runtime undoes any partial effects, records
/// the key, and re-attempts when it appears.
pub enum Activation {
    Active,
    Parked(Key),
}

/// One supervised unit. There is deliberately no `deactivate` to implement:
/// teardown is derived from what `activate` registered, so it cannot drift
/// from the activation it undoes.
#[async_trait]
pub trait Component: Send {
    /// Stable name, unique per runtime. The retire/status handle.
    fn name(&self) -> &str;

    /// Keys this component reads. Declared, not discovered: the ordering
    /// guarantee (dependents deactivate before a provider withdraws) covers
    /// exactly the keys named here.
    fn needs(&self) -> &[Key];

    /// Keys this component publishes. Used for cycle detection before any
    /// activation runs.
    fn provides(&self) -> &[Key];

    /// Do the component's work through `ctx`: read dependencies with
    /// [`Ctx::get`], publish with [`Ctx::provide`], and pair every external
    /// effect with its inverse via [`Ctx::effect`]. A missing dependency is
    /// `Ok(Activation::Parked(key))`, never an `Err`.
    async fn activate(&mut self, ctx: &Ctx) -> Result<Activation>;
}

/// One effect a component performed, in registration order.
enum Effect {
    /// An external action whose inverse the runtime will run.
    Undo(Undo),
    /// A published key. Its "inverse" is removal from the board — positioned
    /// in the same LIFO stack so effects registered before the publication
    /// run after dependents are gone.
    Provided(Key),
}

/// A published value and who published it.
struct Published {
    value: Arc<dyn Any + Send + Sync>,
    /// `Some(fiber index)` when a component provided it, `None` when the
    /// operator (e.g. a health probe) provided it from outside.
    provider: Option<usize>,
}

/// The shared world state. Locked only for short, non-awaiting sections.
#[derive(Default)]
struct Board {
    keys: HashMap<Key, Published>,
    /// The reason the most recent withdrawal of each key gave. Cleared when
    /// the key is provided again. This is what turns "parked" from a mystery
    /// into a sentence an operator can act on.
    withdrawn: HashMap<Key, String>,
    /// Per-fiber effect stacks, indexed like `Runtime::fibers`.
    effects: Vec<Vec<Effect>>,
}

/// The handle a component touches the world through, scoped to one fiber so
/// every effect and publication is attributed to exactly the component that
/// performed it — which is what makes recovery exact.
pub struct Ctx {
    board: Arc<Mutex<Board>>,
    fiber: usize,
}

impl Ctx {
    /// Read a dependency. `None` means the key is not (or no longer) provided
    /// — the caller should return [`Activation::Parked`], not an error. A
    /// value under the key with the wrong type also returns `None`, but is
    /// traced as an error because it is a wiring bug, not an absent provider.
    pub fn get<T: Send + Sync + 'static>(&self, key: &Key) -> Option<Dep<T>> {
        let board = self.board.lock().expect("seam board lock poisoned");
        let provided = board.keys.get(key)?;
        match provided.value.clone().downcast::<T>() {
            Ok(value) => Some(Dep(value)),
            Err(_) => {
                tracing::error!(
                    %key,
                    wanted = std::any::type_name::<T>(),
                    "seam key holds a different type than the consumer asked for"
                );
                None
            }
        }
    }

    /// Publish a value under `key`. The withdrawal is registered as this
    /// component's effect, so deactivation derives it — nobody writes it.
    /// Fails if the key is already provided: two live providers for one key
    /// would make "whose withdrawal parks the dependents?" ambiguous.
    pub fn provide<T: Send + Sync + 'static>(&self, key: Key, value: T) -> Result<()> {
        let mut board = self.board.lock().expect("seam board lock poisoned");
        if board.keys.contains_key(&key) {
            bail!("seam key '{key}' is already provided");
        }
        board.withdrawn.remove(&key);
        board.keys.insert(
            key.clone(),
            Published {
                value: Arc::new(value),
                provider: Some(self.fiber),
            },
        );
        board.effects[self.fiber].push(Effect::Provided(key));
        Ok(())
    }

    /// Register the inverse of an effect just performed. Inverses run LIFO
    /// within a component, and only after every dependent of this component's
    /// keys has deactivated.
    ///
    /// Delegates to [`Ctx::effect_async`] so sync and async inverses share
    /// ONE stack and one `Effect::Undo` variant — that single stack is what
    /// makes LIFO hold across both kinds. (An audit flagged `effect_async` as
    /// dead because nothing outside this module calls it directly; this
    /// delegation is the internal caller that makes it load-bearing.)
    pub fn effect(&self, undo: impl FnOnce() + Send + 'static) {
        self.effect_async(move || {
            undo();
            Box::pin(std::future::ready(())) as BoxFuture<'static, ()>
        })
    }

    /// [`Ctx::effect`] for inverses that must await — the case `Drop`
    /// fundamentally cannot express.
    pub fn effect_async(&self, undo: impl FnOnce() -> BoxFuture<'static, ()> + Send + 'static) {
        let mut board = self.board.lock().expect("seam board lock poisoned");
        board.effects[self.fiber].push(Effect::Undo(Box::new(undo)));
    }

    /// A cheap handle onto one key's presence, usable from inside an inverse
    /// closure (which has no `Ctx`).
    pub fn watch(&self, key: Key) -> KeyWatch {
        KeyWatch {
            board: self.board.clone(),
            key,
        }
    }
}

/// Answers "is this key currently provided, and if not, why not?" without
/// holding any lock across the answer. Cloneable into inverse closures.
#[derive(Clone)]
pub struct KeyWatch {
    board: Arc<Mutex<Board>>,
    key: Key,
}

impl KeyWatch {
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Dep<T>> {
        let board = self.board.lock().expect("seam board lock poisoned");
        let provided = board.keys.get(&self.key)?;
        provided.value.clone().downcast::<T>().ok().map(Dep)
    }

    pub fn is_provided(&self) -> bool {
        self.board
            .lock()
            .expect("seam board lock poisoned")
            .keys
            .contains_key(&self.key)
    }

    /// The reason the last withdrawal gave, if the key is currently withdrawn.
    pub fn park_reason(&self) -> Option<String> {
        self.board
            .lock()
            .expect("seam board lock poisoned")
            .withdrawn
            .get(&self.key)
            .cloned()
    }
}

struct Fiber {
    component: Box<dyn Component>,
    name: String,
    // The declarations, captured ONCE at load (the same idiom as the
    // document-understanding registry): trait methods never run while the
    // runtime is mid-decision, and a component cannot change its answer
    // between scheduling and teardown.
    needs: Vec<Key>,
    provides: Vec<Key>,
    state: FiberState,
    waiting_on: Vec<Key>,
    blocked_by_cycle: bool,
    last_error: Option<String>,
    /// Retired components stay loaded (so status can still name them) but
    /// are never re-activated.
    retired: bool,
}

/// The supervisor. Owns the components and the board; every activation and
/// every inverse runs through it, explicitly — `Drop` is not a mechanism here.
#[derive(Default)]
pub struct Runtime {
    board: Arc<Mutex<Board>>,
    fibers: Vec<Fiber>,
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a component. It does not activate here — [`Runtime::settle`]
    /// activates everything whose declared needs are present.
    pub fn load(&mut self, component: Box<dyn Component>) -> Result<()> {
        let name = component.name().to_string();
        if name.trim().is_empty() {
            bail!("seam component name must be non-empty");
        }
        if self.fibers.iter().any(|fiber| fiber.name == name) {
            bail!("seam component '{name}' is already loaded");
        }
        let needs = component.needs().to_vec();
        let provides = component.provides().to_vec();
        self.board
            .lock()
            .expect("seam board lock poisoned")
            .effects
            .push(Vec::new());
        self.fibers.push(Fiber {
            component,
            name,
            needs,
            provides,
            state: FiberState::Inactive,
            waiting_on: Vec::new(),
            blocked_by_cycle: false,
            last_error: None,
            retired: false,
        });
        Ok(())
    }

    /// Activate every inactive component whose declared needs are present,
    /// to a fixpoint (one activation's `provide` can satisfy the next).
    /// Components left waiting are parked loudly: traced, and visible in
    /// [`Runtime::status`] with the key they wait for. Components whose needs
    /// are satisfiable only through a cycle among loaded components are
    /// reported as such and left inactive — not errored, not deadlocked.
    pub async fn settle(&mut self) {
        // Re-attempted whenever a new key appears within this settle; a fiber
        // that parked or errored is otherwise attempted once, so a persistent
        // failure cannot spin the loop.
        let mut tried: Vec<bool> = vec![false; self.fibers.len()];
        loop {
            let mut progressed = false;
            for i in 0..self.fibers.len() {
                let fiber = &self.fibers[i];
                if tried[i]
                    || fiber.retired
                    || matches!(fiber.state, FiberState::Active | FiberState::Unloading)
                {
                    continue;
                }
                let missing: Vec<Key> = {
                    let board = self.board.lock().expect("seam board lock poisoned");
                    let mut missing: Vec<Key> = fiber
                        .needs
                        .iter()
                        .chain(fiber.waiting_on.iter())
                        .filter(|key| !board.keys.contains_key(key))
                        .cloned()
                        .collect();
                    // `waiting_on` may repeat a declared need after a
                    // withdrawal; the status line must not.
                    let mut seen = Vec::with_capacity(missing.len());
                    missing.retain(|key| {
                        let fresh = !seen.contains(key);
                        seen.push(key.clone());
                        fresh
                    });
                    missing
                };
                if !missing.is_empty() {
                    self.park(i, missing);
                    continue;
                }
                tried[i] = true;
                let ctx = Ctx {
                    board: self.board.clone(),
                    fiber: i,
                };
                match self.fibers[i].component.activate(&ctx).await {
                    Ok(Activation::Active) => {
                        let fiber = &mut self.fibers[i];
                        fiber.state = FiberState::Active;
                        fiber.waiting_on.clear();
                        fiber.last_error = None;
                        tracing::info!(component = fiber.name, "seam component active");
                        // New keys may unblock components already attempted.
                        tried.fill(false);
                        tried[i] = true;
                        progressed = true;
                    }
                    Ok(Activation::Parked(key)) => {
                        // Recovery exactness for a PARTIAL activation: what
                        // it did before discovering the gap is undone now,
                        // not left half-applied until some later teardown.
                        self.run_down_effects(i).await;
                        self.park(i, vec![key]);
                    }
                    Err(error) => {
                        self.run_down_effects(i).await;
                        let fiber = &mut self.fibers[i];
                        fiber.state = FiberState::Inactive;
                        fiber.last_error = Some(format!("{error:#}"));
                        tracing::warn!(
                            component = fiber.name,
                            error = format!("{error:#}"),
                            "seam component failed to activate"
                        );
                    }
                }
            }
            if !progressed {
                break;
            }
        }
        self.mark_cycles();
    }

    /// Provide a key from OUTSIDE the component graph — how a health probe
    /// publishes "the endpoint is up". Settles afterwards, so parked
    /// consumers reactivate in the same call.
    pub async fn provide<T: Send + Sync + 'static>(&mut self, key: Key, value: T) -> Result<()> {
        {
            let mut board = self.board.lock().expect("seam board lock poisoned");
            if board.keys.contains_key(&key) {
                bail!("seam key '{key}' is already provided");
            }
            board.withdrawn.remove(&key);
            board.keys.insert(
                key,
                Published {
                    value: Arc::new(value),
                    provider: None,
                },
            );
        }
        self.settle().await;
        Ok(())
    }

    /// Withdraw a key, with the reason an operator will read. Every active
    /// dependent deactivates FIRST — running its inverses while the key is
    /// still readable — and only then is the key removed. Dependents park
    /// (state [`FiberState::Reloading`]); a later provide reactivates them.
    ///
    /// A key provided by a component is withdrawn by deactivating that
    /// component, so its own inverses also run — in their LIFO position.
    pub async fn withdraw(&mut self, key: &Key, reason: &str) {
        {
            let mut board = self.board.lock().expect("seam board lock poisoned");
            board.withdrawn.insert(key.clone(), reason.to_string());
        }
        let provider = {
            let board = self.board.lock().expect("seam board lock poisoned");
            board.keys.get(key).and_then(|p| p.provider)
        };
        match provider {
            Some(idx) => self.deactivate(idx, false).await,
            None => {
                self.deactivate_dependents(key, reason).await;
                self.board
                    .lock()
                    .expect("seam board lock poisoned")
                    .keys
                    .remove(key);
            }
        }
    }

    /// Retire one component: run ITS inverses (dependents of its keys
    /// deactivate first) and never re-activate it. Nothing else is touched —
    /// that exactness is the point.
    pub async fn retire(&mut self, name: &str) -> Result<()> {
        let Some(idx) = self.fibers.iter().position(|fiber| fiber.name == name) else {
            bail!("no seam component named '{name}' is loaded");
        };
        self.fibers[idx].retired = true;
        self.deactivate(idx, false).await;
        Ok(())
    }

    /// The operator's view: every component, its state, and — for parked
    /// ones — the key it waits for and the withdrawer's reason.
    pub fn status(&self) -> Vec<FiberStatus> {
        let board = self.board.lock().expect("seam board lock poisoned");
        self.fibers
            .iter()
            .map(|fiber| FiberStatus {
                name: fiber.name.clone(),
                state: fiber.state,
                waiting_on: fiber.waiting_on.clone(),
                park_reasons: fiber
                    .waiting_on
                    .iter()
                    .filter_map(|key| {
                        board
                            .withdrawn
                            .get(key)
                            .map(|reason| format!("{key}: {reason}"))
                    })
                    .collect(),
                blocked_by_cycle: fiber.blocked_by_cycle,
                last_error: fiber.last_error.clone(),
            })
            .collect()
    }

    /// A [`KeyWatch`] independent of any component, for callers outside the
    /// graph (the health probe, a status command).
    pub fn watch(&self, key: Key) -> KeyWatch {
        KeyWatch {
            board: self.board.clone(),
            key,
        }
    }

    fn park(&mut self, idx: usize, missing: Vec<Key>) {
        let fiber = &mut self.fibers[idx];
        // Loud on purpose — a parked component that surfaces nowhere is a
        // silent stall, which is worse than the degradation it replaced — but
        // only on a CHANGE, so settle's fixpoint passes do not spam the log.
        if fiber.waiting_on != missing {
            let keys: Vec<String> = missing.iter().map(|k| k.to_string()).collect();
            tracing::warn!(
                component = fiber.name,
                waiting_on = keys.join(", "),
                "seam component parked"
            );
        }
        fiber.waiting_on = missing;
    }

    /// Deactivate fiber `idx`: dependents of every key it provided go first
    /// (the ordering theorem), then its own effect stack runs LIFO. `parked`
    /// says whether this teardown was caused by a dependency withdrawing —
    /// those fibers go to [`FiberState::Reloading`] and reactivate later.
    ///
    /// Boxed because deactivation recurses through the dependency graph;
    /// cycles cannot occur here because cyclic components never activate.
    fn deactivate(&mut self, idx: usize, parked: bool) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if !matches!(self.fibers[idx].state, FiberState::Active) {
                return;
            }
            self.fibers[idx].state = FiberState::Unloading;

            // Dependents first. Withdraw reasons for the keys this teardown
            // takes with it, so a transitively-parked component's status
            // still explains itself.
            let provided: Vec<Key> = {
                let board = self.board.lock().expect("seam board lock poisoned");
                board
                    .keys
                    .iter()
                    .filter(|(_, p)| p.provider == Some(idx))
                    .map(|(key, _)| key.clone())
                    .collect()
            };
            let name = self.fibers[idx].name.clone();
            for key in &provided {
                let reason = format!("provider '{name}' deactivated");
                {
                    let mut board = self.board.lock().expect("seam board lock poisoned");
                    board.withdrawn.entry(key.clone()).or_insert(reason.clone());
                }
                self.deactivate_dependents(key, &reason).await;
            }

            self.run_down_effects(idx).await;

            let fiber = &mut self.fibers[idx];
            fiber.state = if parked {
                FiberState::Reloading
            } else {
                FiberState::Inactive
            };
            tracing::info!(component = fiber.name, parked, "seam component deactivated");
        })
    }

    /// Deactivate every ACTIVE fiber whose declared needs include `key`. The
    /// key itself is untouched — callers remove it (or pop its `Provided`
    /// effect) afterwards, which is exactly what lets a consumer keep reading
    /// it through its own deactivation.
    fn deactivate_dependents<'a>(&'a mut self, key: &'a Key, reason: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            for i in 0..self.fibers.len() {
                let fiber = &self.fibers[i];
                if matches!(fiber.state, FiberState::Active) && fiber.needs.contains(key) {
                    self.deactivate(i, true).await;
                    self.fibers[i].waiting_on = vec![key.clone()];
                    tracing::warn!(
                        component = self.fibers[i].name,
                        key = %key,
                        reason,
                        "seam component parked: its dependency withdrew"
                    );
                }
            }
        })
    }

    /// Run fiber `idx`'s effect stack, LIFO. A `Provided` entry removes the
    /// key from the board at its stack position, so inverses registered
    /// before a publication run after it is gone.
    ///
    /// Each inverse runs under `catch_unwind` (audit F5): without it, one
    /// panicking inverse abandoned every inverse below it on the stack AND
    /// left the fiber permanently `Unloading` — filtered out by `settle`,
    /// short-circuited by `deactivate`, unrecoverable. That made this module
    /// WORSE than the `Drop` guards it replaces, because a panic in one
    /// field's drop still drops the remaining fields. What the catch
    /// delivers is exactly that parity — the remaining inverses run and the
    /// caller still reaches a terminal fiber state — and nothing more: it
    /// does NOT rescue the process from whatever poisoned state made the
    /// inverse panic (if the understanding registry's lock is poisoned, the
    /// next `registry()` call panics all the same). Note also the
    /// `board.lock().expect(...)` calls in this function sit OUTSIDE the
    /// catch: a poisoned BOARD lock still strands the fiber `Unloading` —
    /// accepted, because a poisoned board fails every seam operation anyway.
    /// A caught panic is traced with the component's name.
    async fn run_down_effects(&mut self, idx: usize) {
        use futures::FutureExt as _;
        loop {
            // Pop under the lock, run outside it: an inverse may await.
            let effect = {
                let mut board = self.board.lock().expect("seam board lock poisoned");
                board.effects[idx].pop()
            };
            match effect {
                None => break,
                Some(Effect::Provided(key)) => {
                    let mut board = self.board.lock().expect("seam board lock poisoned");
                    board.keys.remove(&key);
                    let name = &self.fibers[idx].name;
                    board
                        .withdrawn
                        .entry(key)
                        .or_insert_with(|| format!("provider '{name}' deactivated"));
                }
                Some(Effect::Undo(undo)) => {
                    // Two stages, both caught: calling the `FnOnce` (where a
                    // sync inverse registered via `Ctx::effect` actually
                    // runs) and awaiting the future it returned.
                    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(undo));
                    let panicked = match ran {
                        Ok(future) => std::panic::AssertUnwindSafe(future)
                            .catch_unwind()
                            .await
                            .err(),
                        Err(payload) => Some(payload),
                    };
                    if let Some(payload) = panicked {
                        tracing::error!(
                            component = self.fibers[idx].name,
                            panic = panic_text(payload.as_ref()),
                            "an inverse panicked; the remaining inverses still run"
                        );
                    }
                }
            }
        }
    }

    /// Mark components whose needs are satisfiable only through a dependency
    /// cycle among loaded components. Detectable from declarations alone:
    /// optimistically assume every key either present, provided by an
    /// activatable component, or external (declared by no one — it may yet
    /// arrive); whatever still cannot activate is mutually blocked.
    fn mark_cycles(&mut self) {
        let present: Vec<Key> = {
            let board = self.board.lock().expect("seam board lock poisoned");
            board.keys.keys().cloned().collect()
        };
        // Retired fibers are excluded (audit F6): the satisfiability loop
        // below never lets a retired fiber become activatable, so counting
        // its `provides` here declared keys nobody can ever supply from
        // inside the graph — and a consumer of a retired provider's key was
        // reported as cycle-blocked when there is no cycle at all. Such a
        // consumer is merely PARKED: it activates the moment anyone provides
        // the key from outside, which is exactly how the vision key arrives.
        // (For the one component the CLI currently loads this filter is
        // inert — the vision reader's `provides()` is empty, so it declares
        // nothing to exclude; it matters for any graph whose providers are
        // components.)
        let declared: Vec<&Key> = self
            .fibers
            .iter()
            .filter(|f| !f.retired)
            .flat_map(|f| f.provides.iter())
            .collect();
        let mut activatable: Vec<bool> = self
            .fibers
            .iter()
            .map(|f| matches!(f.state, FiberState::Active))
            .collect();
        loop {
            let mut progressed = false;
            for i in 0..self.fibers.len() {
                if activatable[i] || self.fibers[i].retired {
                    continue;
                }
                let satisfiable = self.fibers[i].needs.iter().all(|key| {
                    present.contains(key)
                        || !declared.contains(&key)
                        || self
                            .fibers
                            .iter()
                            .enumerate()
                            .any(|(j, f)| activatable[j] && f.provides.contains(key))
                });
                if satisfiable {
                    activatable[i] = true;
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        for (i, fiber) in self.fibers.iter_mut().enumerate() {
            let blocked = !activatable[i] && !fiber.retired;
            if blocked && !fiber.blocked_by_cycle {
                tracing::warn!(
                    component = fiber.name,
                    "seam component is blocked by a dependency cycle; it stays inactive"
                );
            }
            fiber.blocked_by_cycle = blocked;
        }
    }
}

/// The human-readable text of a panic payload, for the trace line an
/// operator reads when an inverse panics.
fn panic_text(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type OnActivate = Box<dyn FnMut(&Ctx) -> Result<Activation> + Send>;

    /// A component scripted by a closure, so each test states its behaviour
    /// inline instead of through a zoo of fixture types.
    struct Scripted {
        name: &'static str,
        needs: Vec<Key>,
        provides: Vec<Key>,
        on_activate: OnActivate,
    }

    #[async_trait]
    impl Component for Scripted {
        fn name(&self) -> &str {
            self.name
        }
        fn needs(&self) -> &[Key] {
            &self.needs
        }
        fn provides(&self) -> &[Key] {
            &self.provides
        }
        async fn activate(&mut self, ctx: &Ctx) -> Result<Activation> {
            (self.on_activate)(ctx)
        }
    }

    fn scripted(
        name: &'static str,
        needs: &[Key],
        provides: &[Key],
        on_activate: impl FnMut(&Ctx) -> Result<Activation> + Send + 'static,
    ) -> Box<Scripted> {
        Box::new(Scripted {
            name,
            needs: needs.to_vec(),
            provides: provides.to_vec(),
            on_activate: Box::new(on_activate),
        })
    }

    fn log() -> Arc<Mutex<Vec<String>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn push(log: &Arc<Mutex<Vec<String>>>, entry: impl Into<String>) {
        log.lock().unwrap().push(entry.into());
    }

    const K: Key = Key::new("test.key");

    /// THE parking semantic: a missing dependency is not an error. The
    /// component stays inactive, records what it waits for, its activate is
    /// never called — and the moment the key appears it activates, in the
    /// same `provide` call.
    #[tokio::test]
    async fn a_missing_dependency_parks_and_the_component_activates_when_it_appears() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen = attempts.clone();
        let mut rt = Runtime::new();
        rt.load(scripted("consumer", &[K], &[], move |ctx| {
            seen.fetch_add(1, Ordering::SeqCst);
            let value = ctx
                .get::<u32>(&K)
                .expect("settle only runs activate with needs present");
            assert_eq!(*value, 7);
            Ok(Activation::Active)
        }))
        .unwrap();

        rt.settle().await;
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            0,
            "activate must never run while a declared need is absent",
        );
        let status = &rt.status()[0];
        assert_eq!(status.state, FiberState::Inactive);
        assert_eq!(status.waiting_on, vec![K], "the wait must be recorded");

        rt.provide(K, 7u32).await.unwrap();
        assert_eq!(rt.status()[0].state, FiberState::Active);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(rt.status()[0].waiting_on.is_empty());
    }

    /// Parking must be OBSERVABLE — the single biggest risk of this design is
    /// trading silent degradation for silent stalling. A withdrawn key's
    /// reason must reach the parked component's status verbatim.
    #[tokio::test]
    async fn a_parked_component_names_its_key_and_the_withdrawal_reason() {
        let mut rt = Runtime::new();
        rt.load(scripted("consumer", &[K], &[], |_| Ok(Activation::Active)))
            .unwrap();
        // The probe found the endpoint dead before anyone ever provided it.
        rt.withdraw(&K, "probe: connection refused").await;
        rt.settle().await;

        let status = &rt.status()[0];
        assert_eq!(status.state, FiberState::Inactive);
        assert_eq!(status.waiting_on, vec![K]);
        assert_eq!(
            status.park_reasons,
            vec!["test.key: probe: connection refused".to_string()],
            "the operator must see WHY, not just that something waits",
        );
        // And the rendered line carries all of it.
        let line = status.to_string();
        assert!(line.contains("waiting for test.key"), "{line}");
        assert!(line.contains("connection refused"), "{line}");
    }

    /// Inverses run LIFO within one component — the order `Drop` gives a
    /// lexical scope, kept without destroying the component.
    #[tokio::test]
    async fn inverses_run_lifo_within_a_component() {
        let events = log();
        let seen = events.clone();
        let mut rt = Runtime::new();
        rt.load(scripted("worker", &[], &[], move |ctx| {
            for step in ["first", "second", "third"] {
                let events = seen.clone();
                ctx.effect(move || push(&events, format!("undo {step}")));
            }
            Ok(Activation::Active)
        }))
        .unwrap();
        rt.settle().await;
        rt.retire("worker").await.unwrap();

        assert_eq!(
            *events.lock().unwrap(),
            ["undo third", "undo second", "undo first"],
            "inverses must run in reverse registration order",
        );
    }

    /// The ordering theorem, both halves: a consumer's inverses run BEFORE
    /// the provider's, and the consumer can still read the key throughout its
    /// own deactivation. The tool-server pool guarantees this today with
    /// struct field declaration order; here it must hold for any graph.
    #[tokio::test]
    async fn dependents_deactivate_before_the_provider_withdraws() {
        let events = log();
        let mut rt = Runtime::new();

        let seen = events.clone();
        rt.load(scripted("provider", &[], &[K], move |ctx| {
            let events = seen.clone();
            // Registered BEFORE the publication, so LIFO runs it after the
            // key (and its dependents) are gone — the "kill the server only
            // after the flushes" shape.
            ctx.effect(move || push(&events, "provider undo"));
            ctx.provide(K, 7u32)?;
            Ok(Activation::Active)
        }))
        .unwrap();

        let seen = events.clone();
        rt.load(scripted("consumer", &[K], &[], move |ctx| {
            let events = seen.clone();
            let watch = ctx.watch(K);
            ctx.effect(move || {
                let readable = watch.get::<u32>().is_some();
                push(&events, format!("consumer undo (key readable: {readable})"));
            });
            Ok(Activation::Active)
        }))
        .unwrap();

        rt.settle().await;
        assert!(rt.status().iter().all(|s| s.state == FiberState::Active));

        rt.withdraw(&K, "endpoint degraded").await;
        assert_eq!(
            *events.lock().unwrap(),
            ["consumer undo (key readable: true)", "provider undo"],
            "consumer teardown must complete, with the key still readable, \
             before the provider's own inverses run",
        );
        // The consumer parked — it reactivates when the key returns; the
        // provider was deactivated outright.
        let status = rt.status();
        assert_eq!(status[0].state, FiberState::Inactive, "{:?}", status[0]);
        assert_eq!(status[1].state, FiberState::Reloading, "{:?}", status[1]);
        assert_eq!(status[1].waiting_on, vec![K]);
    }

    /// Recovery exactness: retiring one component runs ITS inverses and
    /// nothing else. An unrelated component keeps its keys, its state, and
    /// its unrun inverses.
    #[tokio::test]
    async fn retiring_a_component_withdraws_its_contribution_and_nothing_else() {
        const K2: Key = Key::new("test.other");
        let events = log();
        let mut rt = Runtime::new();

        let seen = events.clone();
        rt.load(scripted("doomed", &[], &[K], move |ctx| {
            let events = seen.clone();
            ctx.effect(move || push(&events, "doomed undo"));
            ctx.provide(K, 1u32)?;
            Ok(Activation::Active)
        }))
        .unwrap();
        let seen = events.clone();
        rt.load(scripted("bystander", &[], &[K2], move |ctx| {
            let events = seen.clone();
            ctx.effect(move || push(&events, "bystander undo"));
            ctx.provide(K2, 2u32)?;
            Ok(Activation::Active)
        }))
        .unwrap();
        rt.settle().await;

        rt.retire("doomed").await.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            ["doomed undo"],
            "only the retired component's inverses may run",
        );
        assert!(
            rt.watch(K2).is_provided(),
            "an unrelated key must survive a retirement untouched",
        );
        assert!(!rt.watch(K).is_provided(), "the retired key is withdrawn");
        assert_eq!(rt.status()[1].state, FiberState::Active);

        // Retired means retired: a later settle must not resurrect it.
        rt.settle().await;
        assert_eq!(rt.status()[0].state, FiberState::Inactive);
        assert_eq!(*events.lock().unwrap(), ["doomed undo"]);
    }

    /// Withdraw → Reloading → the key returns → the component reactivates.
    /// The full park/reload cycle a flapping health probe would drive.
    #[tokio::test]
    async fn a_reloading_component_reactivates_when_the_key_returns() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let seen = attempts.clone();
        let mut rt = Runtime::new();
        rt.load(scripted("consumer", &[K], &[], move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Activation::Active)
        }))
        .unwrap();

        rt.provide(K, 1u32).await.unwrap();
        rt.withdraw(&K, "probe: 503").await;
        assert_eq!(rt.status()[0].state, FiberState::Reloading);

        rt.provide(K, 2u32).await.unwrap();
        assert_eq!(rt.status()[0].state, FiberState::Active);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "reactivation must run activate again, not replay stale state",
        );
    }

    /// A dependency cycle is detectable from the declarations alone. It is
    /// reported and the members stay inactive — no error, no deadlock, and a
    /// component waiting on a genuinely EXTERNAL key is not swept into the
    /// verdict.
    #[tokio::test]
    async fn a_dependency_cycle_is_reported_and_left_inactive() {
        const X: Key = Key::new("cycle.x");
        const Y: Key = Key::new("cycle.y");
        let mut rt = Runtime::new();
        rt.load(scripted("a", &[X], &[Y], |_| Ok(Activation::Active)))
            .unwrap();
        rt.load(scripted("b", &[Y], &[X], |_| Ok(Activation::Active)))
            .unwrap();
        // Waits on a key no loaded component declares — the world may yet
        // provide it, so this one is parked, not cycle-blocked.
        rt.load(scripted("external", &[K], &[], |_| Ok(Activation::Active)))
            .unwrap();

        rt.settle().await; // must return — the cycle cannot deadlock settle
        let status = rt.status();
        assert!(status[0].blocked_by_cycle, "{:?}", status[0]);
        assert!(status[1].blocked_by_cycle, "{:?}", status[1]);
        assert_eq!(status[0].state, FiberState::Inactive);
        assert_eq!(status[1].state, FiberState::Inactive);
        assert!(
            !status[2].blocked_by_cycle,
            "an externally-satisfiable wait is not a cycle: {:?}",
            status[2],
        );
    }

    /// An inverse may await — the case `Drop` fundamentally cannot express,
    /// and the reason the runtime runs inverses explicitly.
    #[tokio::test]
    async fn an_async_inverse_is_awaited_to_completion() {
        let events = log();
        let seen = events.clone();
        let mut rt = Runtime::new();
        rt.load(scripted("worker", &[], &[], move |ctx| {
            let events = seen.clone();
            ctx.effect_async(move || {
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    push(&events, "async undo done");
                })
            });
            Ok(Activation::Active)
        }))
        .unwrap();
        rt.settle().await;
        rt.retire("worker").await.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            ["async undo done"],
            "retire must not return before the async inverse has finished",
        );
    }

    /// A partial activation that parks (or fails) is rolled back NOW: the
    /// effects it registered before hitting the gap run immediately, so
    /// nothing stays half-applied while the component waits.
    #[tokio::test]
    async fn a_partial_activation_is_rolled_back_when_it_parks_or_fails() {
        let events = log();
        let seen = events.clone();
        let mut rt = Runtime::new();
        rt.load(scripted("half", &[], &[], move |ctx| {
            let events = seen.clone();
            ctx.effect(move || push(&events, "half undone"));
            // Discovered mid-activation: an undeclared key is missing.
            Ok(Activation::Parked(K))
        }))
        .unwrap();
        let seen = events.clone();
        rt.load(scripted("broken", &[], &[], move |ctx| {
            let events = seen.clone();
            ctx.effect(move || push(&events, "broken undone"));
            anyhow::bail!("activation exploded")
        }))
        .unwrap();

        rt.settle().await; // must terminate despite the always-failing fiber
        assert_eq!(*events.lock().unwrap(), ["half undone", "broken undone"]);
        let status = rt.status();
        assert_eq!(status[0].waiting_on, vec![K], "the park is recorded");
        assert_eq!(
            status[1].last_error.as_deref(),
            Some("activation exploded"),
            "the failure is recorded, not retried forever",
        );
    }

    /// One key has at most one live provider — otherwise "whose withdrawal
    /// parks the dependents?" has no answer.
    #[tokio::test]
    async fn a_taken_key_refuses_a_second_provider() {
        let mut rt = Runtime::new();
        rt.load(scripted("first", &[], &[K], |ctx| {
            ctx.provide(K, 1u32)?;
            Ok(Activation::Active)
        }))
        .unwrap();
        rt.load(scripted("second", &[], &[K], |ctx| {
            ctx.provide(K, 2u32)?;
            Ok(Activation::Active)
        }))
        .unwrap();
        rt.settle().await;

        let status = rt.status();
        assert_eq!(status[0].state, FiberState::Active);
        assert_eq!(status[1].state, FiberState::Inactive);
        assert!(
            status[1]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("already provided")),
            "{:?}",
            status[1],
        );
        // External provide over a taken key is refused the same way.
        let err = rt.provide(K, 3u32).await.expect_err("key is taken");
        assert!(format!("{err:#}").contains("already provided"));
    }

    /// Audit F5: a panicking inverse must not abandon the rest of the stack
    /// or strand the fiber in `Unloading` forever. `Drop` gets this right —
    /// a panic in one field's drop still drops the remaining fields — so the
    /// seam must not be worse than the mechanism it replaces. Reachable in
    /// production: the vision tombstone inverse `.expect()`s on a poisoned
    /// registry lock.
    #[tokio::test]
    async fn a_panicking_inverse_does_not_abandon_the_stack_or_strand_the_fiber() {
        let events = log();
        let seen = events.clone();
        let mut rt = Runtime::new();
        rt.load(scripted("consumer", &[K], &[], move |ctx| {
            let events = seen.clone();
            // Registered FIRST, so LIFO runs it LAST — after both panics.
            ctx.effect(move || push(&events, "good undone"));
            ctx.effect_async(|| Box::pin(async { panic!("async inverse exploded") }));
            ctx.effect(|| panic!("sync inverse exploded"));
            Ok(Activation::Active)
        }))
        .unwrap();
        rt.provide(K, 1u32).await.unwrap();
        assert_eq!(rt.status()[0].state, FiberState::Active);

        // Both panicking inverses run (and are caught) before the good one.
        rt.withdraw(&K, "probe: 503").await;
        assert_eq!(
            *events.lock().unwrap(),
            ["good undone"],
            "the inverse below the panicking ones must still run",
        );
        // Terminal state was reached on the panic path: the fiber is parked
        // (Reloading), not stranded Unloading where settle would skip it.
        assert_eq!(rt.status()[0].state, FiberState::Reloading);

        // And the fiber is still alive to the seam: the key returning
        // reactivates it exactly as after a clean teardown.
        rt.provide(K, 2u32).await.unwrap();
        assert_eq!(
            rt.status()[0].state,
            FiberState::Active,
            "a caught panic must not cost the component its future",
        );
    }

    /// Audit F6: a key whose only declarer was RETIRED is external now — its
    /// consumer is parked, not cycle-blocked. Before the fix, `mark_cycles`
    /// collected `declared` from all fibers but let only non-retired ones
    /// become activatable, so retire(provider) → settle() reported a
    /// dependency cycle where none exists.
    #[tokio::test]
    async fn a_retired_providers_key_is_not_reported_as_a_cycle() {
        let mut rt = Runtime::new();
        rt.load(scripted("provider", &[], &[K], |ctx| {
            ctx.provide(K, 1u32)?;
            Ok(Activation::Active)
        }))
        .unwrap();
        rt.load(scripted("consumer", &[K], &[], |_| Ok(Activation::Active)))
            .unwrap();
        rt.settle().await;
        rt.retire("provider").await.unwrap();
        rt.settle().await;

        let consumer = &rt.status()[1];
        assert!(
            !consumer.blocked_by_cycle,
            "a consumer of a retired provider's key is parked, not \
             cycle-blocked: {consumer:?}",
        );
        assert_eq!(consumer.waiting_on, vec![K], "the wait is still recorded");

        // The proof there was no cycle: the key arriving from OUTSIDE (how
        // the vision endpoint key arrives) activates the consumer at once.
        rt.provide(K, 2u32).await.unwrap();
        assert_eq!(rt.status()[1].state, FiberState::Active);
    }

    /// `load` must refuse an empty name: every later lookup (`retire`,
    /// `status`, `vision_deferral_reason` downstream) is by name, and an
    /// unnameable component can never be retired or explained.
    #[tokio::test]
    async fn a_component_with_an_empty_name_is_refused() {
        let mut rt = Runtime::new();
        let err = rt
            .load(scripted("  ", &[], &[], |_| Ok(Activation::Active)))
            .expect_err("an unnameable component must be refused");
        assert!(format!("{err:#}").contains("non-empty"), "{err:#}");
        assert!(rt.status().is_empty(), "nothing may have been loaded");
    }

    /// `load` must refuse a duplicate name: `retire` uses `position()` and
    /// the CLI's park lookup uses `.find()`, both first-match, so a second
    /// fiber under the same name would be silently unreachable by either.
    #[tokio::test]
    async fn a_duplicate_component_name_is_refused() {
        let mut rt = Runtime::new();
        rt.load(scripted("worker", &[], &[], |_| Ok(Activation::Active)))
            .unwrap();
        let err = rt
            .load(scripted("worker", &[], &[], |_| Ok(Activation::Active)))
            .expect_err("a shadowed name must be refused");
        assert!(format!("{err:#}").contains("already loaded"), "{err:#}");
        assert_eq!(rt.status().len(), 1, "the first load stands alone");
    }

    /// `retire` of an unknown name must be an error. An audit replaced this
    /// bail with `return Ok(())` — a typo'd retire silently reporting
    /// success while the component it meant kept running — and nothing
    /// failed.
    #[tokio::test]
    async fn retiring_an_unknown_component_is_refused() {
        let mut rt = Runtime::new();
        rt.load(scripted("worker", &[], &[], |_| Ok(Activation::Active)))
            .unwrap();
        rt.settle().await;
        let err = rt
            .retire("wroker")
            .await
            .expect_err("a typo'd name must not report success");
        assert!(format!("{err:#}").contains("wroker"), "{err:#}");
        // And the component it failed to name is untouched.
        assert_eq!(rt.status()[0].state, FiberState::Active);
    }
}
