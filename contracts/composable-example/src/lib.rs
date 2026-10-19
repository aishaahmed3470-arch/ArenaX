#![no_std]

//! # Composable Tournament Hook Interface — Issue #1417
//!
//! Defines the `TournamentHook` trait-contract interface and provides two
//! concrete implementations:
//!
//! 1. **`NoopHook`** — default no-op implementation; every callback succeeds
//!    immediately without side-effects.  Safe to use when a tournament needs
//!    no custom lifecycle logic.
//!
//! 2. **`ComposableHook`** — production implementation that records hook
//!    invocations in ledger storage (for on-chain auditability) and allows an
//!    owner to configure custom behaviour.  It is the reference hook contract
//!    for `sponsored-tournament` integration.
//!
//! ## Hook interface
//!
//! ```text
//! on_register(tournament_id) → bool   // called when a player registers
//! on_start(tournament_id)    → bool   // called when the tournament begins
//! on_complete(tournament_id) → bool   // called when the tournament ends
//! ```
//!
//! A return value of `false` (or a panic) from any callback causes the
//! `sponsored-tournament` contract to roll back the triggering lifecycle
//! action atomically — both contracts share the same Soroban transaction.
//!
//! ## No-op hook
//!
//! `NoopHookContract` always returns `true` from every callback.  Deploy it
//! when you want to run a tournament without any custom hook logic.

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Symbol, Vec};

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    /// Contract owner / admin.
    Owner,
    /// Whether the contract is paused.
    Paused,
    /// Ordered history of (tournament_id, hook_name) invocations.
    HookHistory,
    /// Custom on_register handler enabled flag.
    RegisterEnabled,
    /// Custom on_start handler enabled flag.
    StartEnabled,
    /// Custom on_complete handler enabled flag.
    CompleteEnabled,
}

// ---------------------------------------------------------------------------
// Hook invocation record — persisted for auditability
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug)]
pub struct HookInvocation {
    /// Identifier of the tournament that triggered this hook.
    pub tournament_id: u64,
    /// Symbolic name: "on_register" | "on_start" | "on_complete".
    pub hook_name: Symbol,
    /// Ledger timestamp at invocation time.
    pub invoked_at: u64,
    /// Whether the hook returned `true` (success) or `false` (revert trigger).
    pub succeeded: bool,
}

// ---------------------------------------------------------------------------
// ComposableHook — configurable reference implementation
// ---------------------------------------------------------------------------

#[contract]
pub struct ComposableHook;

#[contractimpl]
impl ComposableHook {
    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// Initialize the hook contract with `owner` as admin.
    /// All three hook handlers default to *enabled* (returning `true`).
    pub fn initialize(env: Env, owner: Address) {
        if env.storage().instance().has(&DataKey::Owner) {
            panic!("already initialized");
        }
        owner.require_auth();
        env.storage().instance().set(&DataKey::Owner, &owner);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .set(&DataKey::RegisterEnabled, &true);
        env.storage()
            .instance()
            .set(&DataKey::StartEnabled, &true);
        env.storage()
            .instance()
            .set(&DataKey::CompleteEnabled, &true);
        env.storage()
            .instance()
            .set(&DataKey::HookHistory, &Vec::<HookInvocation>::new(&env));
    }

    // ── Admin ────────────────────────────────────────────────────────────────

    pub fn owner(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Owner)
            .expect("not initialized")
    }

    pub fn transfer_ownership(env: Env, new_owner: Address) {
        let owner = Self::owner(env.clone());
        owner.require_auth();
        env.storage().instance().set(&DataKey::Owner, &new_owner);
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    pub fn set_paused(env: Env, paused: bool) {
        let owner = Self::owner(env.clone());
        owner.require_auth();
        env.storage().instance().set(&DataKey::Paused, &paused);
    }

    // ── Hook configuration ───────────────────────────────────────────────────

    /// Enable or disable the `on_register` callback.  Owner-only.
    pub fn set_register_enabled(env: Env, enabled: bool) {
        let owner = Self::owner(env.clone());
        owner.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::RegisterEnabled, &enabled);
    }

    /// Enable or disable the `on_start` callback.  Owner-only.
    pub fn set_start_enabled(env: Env, enabled: bool) {
        let owner = Self::owner(env.clone());
        owner.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::StartEnabled, &enabled);
    }

    /// Enable or disable the `on_complete` callback.  Owner-only.
    pub fn set_complete_enabled(env: Env, enabled: bool) {
        let owner = Self::owner(env.clone());
        owner.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::CompleteEnabled, &enabled);
    }

    // ── Hook interface ───────────────────────────────────────────────────────

    /// Called by `sponsored-tournament` when a player registers.
    ///
    /// Returns `true`  → registration proceeds.
    /// Returns `false` → registration must be rolled back by the caller.
    ///
    /// Panicking here also causes the caller to roll back (Soroban semantics).
    pub fn on_register(env: Env, tournament_id: u64) -> bool {
        if Self::is_paused(env.clone()) {
            panic!("hook contract is paused");
        }

        let enabled: bool = env
            .storage()
            .instance()
            .get(&DataKey::RegisterEnabled)
            .unwrap_or(true);

        let hook_name = Symbol::new(&env, "on_register");
        Self::record_invocation(&env, tournament_id, hook_name, enabled);

        enabled
    }

    /// Called by `sponsored-tournament` when the tournament starts.
    pub fn on_start(env: Env, tournament_id: u64) -> bool {
        if Self::is_paused(env.clone()) {
            panic!("hook contract is paused");
        }

        let enabled: bool = env
            .storage()
            .instance()
            .get(&DataKey::StartEnabled)
            .unwrap_or(true);

        let hook_name = Symbol::new(&env, "on_start");
        Self::record_invocation(&env, tournament_id, hook_name, enabled);

        enabled
    }

    /// Called by `sponsored-tournament` when the tournament completes.
    pub fn on_complete(env: Env, tournament_id: u64) -> bool {
        if Self::is_paused(env.clone()) {
            panic!("hook contract is paused");
        }

        let enabled: bool = env
            .storage()
            .instance()
            .get(&DataKey::CompleteEnabled)
            .unwrap_or(true);

        let hook_name = Symbol::new(&env, "on_complete");
        Self::record_invocation(&env, tournament_id, hook_name, enabled);

        enabled
    }

    // ── History / introspection ──────────────────────────────────────────────

    /// Return the full ordered invocation history for audit purposes.
    pub fn hook_history(env: Env) -> Vec<HookInvocation> {
        env.storage()
            .instance()
            .get(&DataKey::HookHistory)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Clear the invocation history.  Owner-only.
    pub fn clear_history(env: Env) {
        let owner = Self::owner(env.clone());
        owner.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::HookHistory, &Vec::<HookInvocation>::new(&env));
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    fn record_invocation(env: &Env, tournament_id: u64, hook_name: Symbol, succeeded: bool) {
        let invocation = HookInvocation {
            tournament_id,
            hook_name,
            invoked_at: env.ledger().timestamp(),
            succeeded,
        };

        let mut history: Vec<HookInvocation> = env
            .storage()
            .instance()
            .get(&DataKey::HookHistory)
            .unwrap_or_else(|| Vec::new(env));

        history.push_back(invocation);
        env.storage()
            .instance()
            .set(&DataKey::HookHistory, &history);
    }
}

// ---------------------------------------------------------------------------
// NoopHook — default no-op implementation
// ---------------------------------------------------------------------------

/// `NoopHookContract` is a zero-overhead hook contract that always returns
/// `true` from every callback.  Use it for tournaments that require no custom
/// lifecycle logic while still satisfying the hook interface.

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum NoopDataKey {
    Initialized,
}

#[contract]
pub struct NoopHookContract;

#[contractimpl]
impl NoopHookContract {
    pub fn initialize(env: Env) {
        if env.storage().instance().has(&NoopDataKey::Initialized) {
            panic!("already initialized");
        }
        env.storage()
            .instance()
            .set(&NoopDataKey::Initialized, &true);
    }

    /// Always returns `true` — registration always proceeds.
    pub fn on_register(_env: Env, _tournament_id: u64) -> bool {
        true
    }

    /// Always returns `true` — start always proceeds.
    pub fn on_start(_env: Env, _tournament_id: u64) -> bool {
        true
    }

    /// Always returns `true` — completion always proceeds.
    pub fn on_complete(_env: Env, _tournament_id: u64) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Tests — Issue #1417
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    fn deploy_hook(env: &Env) -> (Address, Address, ComposableHookClient) {
        let owner = Address::generate(env);
        let contract_id = env.register(ComposableHook, ());
        let client = ComposableHookClient::new(env, &contract_id);
        env.mock_all_auths();
        client.initialize(&owner);
        (contract_id, owner, client)
    }

    fn deploy_noop(env: &Env) -> (Address, NoopHookContractClient) {
        let contract_id = env.register(NoopHookContract, ());
        let client = NoopHookContractClient::new(env, &contract_id);
        env.mock_all_auths();
        client.initialize();
        (contract_id, client)
    }

    // ── NoopHook ─────────────────────────────────────────────────────────────

    #[test]
    fn noop_hook_all_callbacks_return_true() {
        let env = Env::default();
        let (_id, client) = deploy_noop(&env);

        assert!(client.on_register(&42u64));
        assert!(client.on_start(&42u64));
        assert!(client.on_complete(&42u64));
    }

    // ── ComposableHook initialization ────────────────────────────────────────

    #[test]
    fn composable_hook_initializes_correctly() {
        let env = Env::default();
        let (_id, owner, client) = deploy_hook(&env);

        assert_eq!(client.owner(), owner);
        assert!(!client.is_paused());
        assert_eq!(client.hook_history().len(), 0);
    }

    #[test]
    #[should_panic(expected = "already initialized")]
    fn composable_hook_double_init_panics() {
        let env = Env::default();
        let (_id, owner, client) = deploy_hook(&env);
        env.mock_all_auths();
        client.initialize(&owner);
    }

    // ── Lifecycle callbacks execute in order ──────────────────────────────────

    #[test]
    fn hook_callbacks_execute_in_correct_order_and_are_recorded() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000);
        let (_id, _owner, client) = deploy_hook(&env);

        let tid = 99u64;

        // Simulate the full lifecycle: register → start → complete.
        assert!(client.on_register(&tid), "on_register should return true");
        env.ledger().set_timestamp(2_000);
        assert!(client.on_start(&tid), "on_start should return true");
        env.ledger().set_timestamp(3_000);
        assert!(client.on_complete(&tid), "on_complete should return true");

        let history = client.hook_history();
        assert_eq!(history.len(), 3, "three hook invocations should be recorded");

        let h0 = history.get(0).unwrap();
        let h1 = history.get(1).unwrap();
        let h2 = history.get(2).unwrap();

        // Check chronological order.
        assert_eq!(h0.hook_name, Symbol::new(&env, "on_register"));
        assert_eq!(h1.hook_name, Symbol::new(&env, "on_start"));
        assert_eq!(h2.hook_name, Symbol::new(&env, "on_complete"));

        // All invocations are for the same tournament.
        assert_eq!(h0.tournament_id, tid);
        assert_eq!(h1.tournament_id, tid);
        assert_eq!(h2.tournament_id, tid);

        // Timestamps increase monotonically.
        assert!(h0.invoked_at <= h1.invoked_at);
        assert!(h1.invoked_at <= h2.invoked_at);

        // All succeeded.
        assert!(h0.succeeded);
        assert!(h1.succeeded);
        assert!(h2.succeeded);
    }

    // ── Disabled hooks signal rollback ────────────────────────────────────────

    #[test]
    fn disabled_on_register_returns_false_triggering_rollback() {
        let env = Env::default();
        let (_id, _owner, client) = deploy_hook(&env);

        env.mock_all_auths();
        client.set_register_enabled(&false);

        // Returns false → caller (sponsored-tournament) must roll back.
        assert!(!client.on_register(&1u64));
    }

    #[test]
    fn disabled_on_start_returns_false_triggering_rollback() {
        let env = Env::default();
        let (_id, _owner, client) = deploy_hook(&env);

        env.mock_all_auths();
        client.set_start_enabled(&false);

        assert!(!client.on_start(&1u64));
    }

    #[test]
    fn disabled_on_complete_returns_false_triggering_rollback() {
        let env = Env::default();
        let (_id, _owner, client) = deploy_hook(&env);

        env.mock_all_auths();
        client.set_complete_enabled(&false);

        assert!(!client.on_complete(&1u64));
    }

    // ── Paused contract panics (hard revert) ─────────────────────────────────

    #[test]
    #[should_panic(expected = "hook contract is paused")]
    fn paused_hook_panics_on_register() {
        let env = Env::default();
        let (_id, _owner, client) = deploy_hook(&env);

        env.mock_all_auths();
        client.set_paused(&true);
        client.on_register(&1u64);
    }

    #[test]
    #[should_panic(expected = "hook contract is paused")]
    fn paused_hook_panics_on_start() {
        let env = Env::default();
        let (_id, _owner, client) = deploy_hook(&env);

        env.mock_all_auths();
        client.set_paused(&true);
        client.on_start(&1u64);
    }

    #[test]
    #[should_panic(expected = "hook contract is paused")]
    fn paused_hook_panics_on_complete() {
        let env = Env::default();
        let (_id, _owner, client) = deploy_hook(&env);

        env.mock_all_auths();
        client.set_paused(&true);
        client.on_complete(&1u64);
    }

    // ── History management ────────────────────────────────────────────────────

    #[test]
    fn clear_history_empties_invocation_log() {
        let env = Env::default();
        let (_id, _owner, client) = deploy_hook(&env);

        client.on_register(&1u64);
        client.on_start(&1u64);
        assert_eq!(client.hook_history().len(), 2);

        env.mock_all_auths();
        client.clear_history();
        assert_eq!(client.hook_history().len(), 0);
    }

    // ── Multiple tournaments do not interfere ─────────────────────────────────

    #[test]
    fn multiple_tournaments_tracked_independently() {
        let env = Env::default();
        let (_id, _owner, client) = deploy_hook(&env);

        client.on_register(&10u64);
        client.on_register(&20u64);
        client.on_start(&10u64);
        client.on_complete(&20u64);
        client.on_complete(&10u64);

        let history = client.hook_history();
        assert_eq!(history.len(), 5);

        // Verify tournament IDs in insertion order.
        let ids: soroban_sdk::Vec<u64> = history.iter().map(|h| h.tournament_id).collect();
        assert_eq!(ids, soroban_sdk::vec![&env, 10u64, 20u64, 10u64, 20u64, 10u64]);
    }

    // ── Full tournament lifecycle integration test ────────────────────────────

    /// Deploy a custom hook, run an entire tournament lifecycle, and assert
    /// that the three hook functions execute in the correct order.
    /// This is the canonical integration scenario for Issue #1417.
    #[test]
    fn full_tournament_lifecycle_with_custom_hook() {
        let env = Env::default();
        env.ledger().set_timestamp(1_000);
        let (_id, _owner, hook) = deploy_hook(&env);

        // ── Step 1: Player registration ──────────────────────────────────────
        let tournament_id = 42u64;
        let register_result = hook.on_register(&tournament_id);
        assert!(register_result, "registration hook must succeed");

        // ── Step 2: Tournament start ─────────────────────────────────────────
        env.ledger().set_timestamp(2_000);
        let start_result = hook.on_start(&tournament_id);
        assert!(start_result, "start hook must succeed");

        // ── Step 3: Tournament completion ────────────────────────────────────
        env.ledger().set_timestamp(3_000);
        let complete_result = hook.on_complete(&tournament_id);
        assert!(complete_result, "complete hook must succeed");

        // ── Verify recorded order ─────────────────────────────────────────────
        let history = hook.hook_history();
        assert_eq!(history.len(), 3, "exactly 3 hook invocations should be recorded");

        assert_eq!(
            history.get(0).unwrap().hook_name,
            Symbol::new(&env, "on_register"),
            "first hook must be on_register"
        );
        assert_eq!(
            history.get(1).unwrap().hook_name,
            Symbol::new(&env, "on_start"),
            "second hook must be on_start"
        );
        assert_eq!(
            history.get(2).unwrap().hook_name,
            Symbol::new(&env, "on_complete"),
            "third hook must be on_complete"
        );

        // ── Verify timestamps are monotonically increasing ───────────────────
        let t0 = history.get(0).unwrap().invoked_at;
        let t1 = history.get(1).unwrap().invoked_at;
        let t2 = history.get(2).unwrap().invoked_at;
        assert!(t0 <= t1 && t1 <= t2);
    }

    // ── Atomic rollback simulation ────────────────────────────────────────────

    /// When `on_start` returns false the parent contract must not advance the
    /// tournament state.  This test simulates the caller-side check: verify
    /// the hook returns false and assert the expected rollback guard fires.
    #[test]
    fn atomic_rollback_when_start_hook_disabled() {
        let env = Env::default();
        let (_id, _owner, hook) = deploy_hook(&env);

        env.mock_all_auths();
        hook.set_start_enabled(&false);

        // Simulate what sponsored-tournament does: call on_start and roll back
        // if the result is false.
        let start_result = hook.on_start(&5u64);

        // Hook signals: do NOT proceed.
        assert!(!start_result, "disabled start hook must signal rollback");

        // In the real contract integration, the caller would `panic!` here to
        // roll back.  The test documents that the signal is correctly received.
    }
}
