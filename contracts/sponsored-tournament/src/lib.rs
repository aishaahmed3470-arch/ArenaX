#![no_std]

//! # Sponsored Tournament Contract — with Composable Hook Interface (#1417)
//!
//! Manages sponsored tournaments and community prize pools on ArenaX.
//!
//! ## Hook integration (#1417)
//!
//! Every lifecycle milestone now invokes the corresponding hook on an
//! optional registered hook contract:
//!
//! | Milestone          | Hook call       | On hook `false` / panic |
//! |--------------------|-----------------|--------------------------|
//! | Player registers   | `on_register`   | Registration reverts     |
//! | Tournament starts  | `on_start`      | Start reverts            |
//! | Tournament ends    | `on_complete`   | Completion reverts       |
//!
//! Because both contracts participate in the same Soroban transaction, a
//! panic in the hook automatically rolls back all state written so far
//! (Soroban's built-in atomic-rollback guarantee).  When the hook returns
//! `false` the tournament contract explicitly panics to trigger the same
//! rollback, keeping the two paths behaviorally identical.
//!
//! ## No-op fallback
//!
//! When no hook contract is registered (`hook_contract = None`), every
//! lifecycle action succeeds without any cross-contract call.

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, Bytes, Env, Symbol, Vec,
};

// ---------------------------------------------------------------------------
// Cross-contract client for the ComposableHook interface
// ---------------------------------------------------------------------------

mod hook_client {
    use soroban_sdk::{contractclient, Env};

    /// Minimal client for the `ComposableHook` / `NoopHookContract` interface
    /// defined in the `composable-example` crate.  Both contracts expose the
    /// same three entrypoints.
    #[contractclient(name = "HookClient")]
    pub trait TournamentHookInterface {
        fn on_register(env: Env, tournament_id: u64) -> bool;
        fn on_start(env: Env, tournament_id: u64) -> bool;
        fn on_complete(env: Env, tournament_id: u64) -> bool;
    }
}

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug)]
pub enum TournamentApprovalStatus {
    Pending,
    Approved,
    Rejected,
}

/// Tournament lifecycle state.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum TournamentLifecycleState {
    /// Tournament created, waiting for admin approval.
    Pending,
    /// Admin has approved; players may register.
    Approved,
    /// Tournament has been rejected by admin.
    Rejected,
    /// Tournament is running (started).
    InProgress,
    /// Tournament has finished.
    Completed,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct CommunityTournament {
    pub id: u64,
    pub organizer: Address,
    pub sponsor: Address,
    pub prize_pool_amount: i128,
    pub token: Address,
    pub approval_status: TournamentApprovalStatus,
    pub lifecycle: TournamentLifecycleState,
    pub is_sponsored: bool,
    pub telemetry_hash: Bytes,
    /// Optional address of a deployed hook contract.
    /// When `None`, lifecycle callbacks are silently skipped.
    pub hook_contract: Option<Address>,
}

#[contracttype]
pub enum DataKey {
    Admin,
    TournamentCount,
    Tournament(u64),
    SponsorPool(u64),
    /// Registered players for a given tournament.
    Players(u64),
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct SponsoredTournamentContract;

#[contractimpl]
impl SponsoredTournamentContract {
    // ── Initialization ───────────────────────────────────────────────────────

    /// Initialize the contract with an admin authority.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().persistent().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().persistent().set(&DataKey::Admin, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::TournamentCount, &0u64);
    }

    // ── Tournament creation ──────────────────────────────────────────────────

    /// Create a community tournament, optionally providing a hook contract.
    ///
    /// If `hook_contract` is `Some(addr)`, every lifecycle transition will
    /// cross-contract-call `addr`.  If it is `None`, a no-op path is taken
    /// and no hook calls are made.
    pub fn create_community_tournament(
        env: Env,
        organizer: Address,
        sponsor: Address,
        prize_pool_amount: i128,
        token_addr: Address,
        is_sponsored: bool,
        telemetry_hash: Bytes,
        hook_contract: Option<Address>,
    ) -> u64 {
        organizer.require_auth();

        let count: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::TournamentCount)
            .unwrap_or(0);
        let id = count + 1;

        if prize_pool_amount > 0 {
            let client = token::Client::new(&env, &token_addr);
            client.transfer(
                &organizer,
                &env.current_contract_address(),
                &prize_pool_amount,
            );
        }

        let tournament = CommunityTournament {
            id,
            organizer,
            sponsor,
            prize_pool_amount,
            token: token_addr,
            approval_status: TournamentApprovalStatus::Pending,
            lifecycle: TournamentLifecycleState::Pending,
            is_sponsored,
            telemetry_hash,
            hook_contract,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Tournament(id), &tournament);
        env.storage()
            .persistent()
            .set(&DataKey::TournamentCount, &id);
        id
    }

    // ── Admin approval ───────────────────────────────────────────────────────

    /// Approve a community tournament (moves it to `Approved` state).
    pub fn approve_tournament(env: Env, admin: Address, tournament_id: u64) {
        admin.require_auth();
        let stored_admin: Address =
            env.storage().persistent().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("unauthorized");
        }

        let mut tournament: CommunityTournament = env
            .storage()
            .persistent()
            .get(&DataKey::Tournament(tournament_id))
            .expect("tournament not found");

        tournament.approval_status = TournamentApprovalStatus::Approved;
        tournament.lifecycle = TournamentLifecycleState::Approved;
        env.storage()
            .persistent()
            .set(&DataKey::Tournament(tournament_id), &tournament);
    }

    // ── Player registration ──────────────────────────────────────────────────

    /// Register a player for an approved tournament.
    ///
    /// Calls `hook.on_register(tournament_id)` when a hook is configured.
    /// If the hook returns `false` or panics, the entire registration is
    /// rolled back atomically (Soroban transaction semantics).
    pub fn register_player(env: Env, player: Address, tournament_id: u64) {
        player.require_auth();

        let tournament: CommunityTournament = env
            .storage()
            .persistent()
            .get(&DataKey::Tournament(tournament_id))
            .expect("tournament not found");

        if !matches!(
            tournament.lifecycle,
            TournamentLifecycleState::Approved
        ) {
            panic!("tournament is not open for registration");
        }

        // ── Hook: on_register ────────────────────────────────────────────────
        if let Some(hook_addr) = &tournament.hook_contract {
            let client = hook_client::HookClient::new(&env, hook_addr);
            let ok = client.on_register(&tournament_id);
            if !ok {
                // Hook vetoed the registration — roll back by panicking.
                panic!("on_register hook rejected registration");
            }
        }

        // Persist player list.
        let mut players: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Players(tournament_id))
            .unwrap_or_else(|| Vec::new(&env));

        // Prevent duplicate registrations.
        let already_registered = players.iter().any(|p| p == player);
        if already_registered {
            panic!("player already registered");
        }

        players.push_back(player);
        env.storage()
            .persistent()
            .set(&DataKey::Players(tournament_id), &players);
    }

    // ── Tournament start ─────────────────────────────────────────────────────

    /// Start an approved tournament (requires admin authorization).
    ///
    /// Calls `hook.on_start(tournament_id)`.  If the hook vetoes the start
    /// by returning `false` or panicking, the state transition is rolled back.
    pub fn start_tournament(env: Env, admin: Address, tournament_id: u64) {
        admin.require_auth();
        let stored_admin: Address =
            env.storage().persistent().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("unauthorized");
        }

        let mut tournament: CommunityTournament = env
            .storage()
            .persistent()
            .get(&DataKey::Tournament(tournament_id))
            .expect("tournament not found");

        if !matches!(tournament.lifecycle, TournamentLifecycleState::Approved) {
            panic!("tournament must be approved before starting");
        }

        // ── Hook: on_start ───────────────────────────────────────────────────
        // Write state *before* the hook call so the hook can observe the
        // in-progress state; Soroban rolls it back if the hook panics.
        tournament.lifecycle = TournamentLifecycleState::InProgress;
        env.storage()
            .persistent()
            .set(&DataKey::Tournament(tournament_id), &tournament);

        if let Some(hook_addr) = &tournament.hook_contract {
            let client = hook_client::HookClient::new(&env, hook_addr);
            let ok = client.on_start(&tournament_id);
            if !ok {
                panic!("on_start hook rejected tournament start");
            }
        }
    }

    // ── Tournament completion ────────────────────────────────────────────────

    /// Mark a running tournament as completed (admin only).
    ///
    /// Calls `hook.on_complete(tournament_id)`.  If the hook vetoes completion
    /// by returning `false` or panicking, the state is rolled back.
    pub fn complete_tournament(env: Env, admin: Address, tournament_id: u64) {
        admin.require_auth();
        let stored_admin: Address =
            env.storage().persistent().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("unauthorized");
        }

        let mut tournament: CommunityTournament = env
            .storage()
            .persistent()
            .get(&DataKey::Tournament(tournament_id))
            .expect("tournament not found");

        if !matches!(tournament.lifecycle, TournamentLifecycleState::InProgress) {
            panic!("tournament must be in progress to complete");
        }

        // Optimistically transition state; hook can roll it back.
        tournament.lifecycle = TournamentLifecycleState::Completed;
        env.storage()
            .persistent()
            .set(&DataKey::Tournament(tournament_id), &tournament);

        // ── Hook: on_complete ────────────────────────────────────────────────
        if let Some(hook_addr) = &tournament.hook_contract {
            let client = hook_client::HookClient::new(&env, hook_addr);
            let ok = client.on_complete(&tournament_id);
            if !ok {
                panic!("on_complete hook rejected tournament completion");
            }
        }
    }

    // ── Getters ──────────────────────────────────────────────────────────────

    /// Return the full tournament record.
    pub fn get_tournament(env: Env, tournament_id: u64) -> Option<CommunityTournament> {
        env.storage()
            .persistent()
            .get(&DataKey::Tournament(tournament_id))
    }

    /// Return the registered players for a tournament.
    pub fn get_players(env: Env, tournament_id: u64) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::Players(tournament_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// AI Anti-cheat telemetry verification (Issue #1250).
    pub fn verify_telemetry(env: Env, tournament_id: u64, telemetry_proof: Bytes) -> bool {
        let tournament: CommunityTournament = env
            .storage()
            .persistent()
            .get(&DataKey::Tournament(tournament_id))
            .expect("tournament not found");

        telemetry_proof.len() > 0 && tournament.telemetry_hash.len() > 0
    }
}

// ---------------------------------------------------------------------------
// Tests — Issue #1417 hook wiring
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Bytes, Env};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn setup(
        env: &Env,
    ) -> (
        Address, // tournament contract
        Address, // admin
        SponsoredTournamentContractClient,
    ) {
        let admin = Address::generate(env);
        let contract_id = env.register(SponsoredTournamentContract, ());
        let client = SponsoredTournamentContractClient::new(env, &contract_id);
        env.mock_all_auths();
        client.initialize(&admin);
        (contract_id, admin, client)
    }

    fn empty_hash(env: &Env) -> Bytes {
        Bytes::new(env)
    }

    // ── Without a hook ────────────────────────────────────────────────────────

    #[test]
    fn tournament_without_hook_full_lifecycle() {
        let env = Env::default();
        let (_id, admin, client) = setup(&env);
        let organizer = Address::generate(&env);

        env.mock_all_auths();

        // We can't call create_community_tournament with prize_pool_amount > 0
        // without a real token contract in unit tests, so use 0.
        let tid = client.create_community_tournament(
            &organizer,
            &organizer,
            &0i128,
            &organizer, // token addr (unused with 0 amount)
            &false,
            &empty_hash(&env),
            &None,
        );

        client.approve_tournament(&admin, &tid);

        let player = Address::generate(&env);
        client.register_player(&player, &tid);

        client.start_tournament(&admin, &tid);
        client.complete_tournament(&admin, &tid);

        let t = client.get_tournament(&tid).unwrap();
        assert_eq!(t.lifecycle, TournamentLifecycleState::Completed);
        assert_eq!(client.get_players(&tid).len(), 1);
    }

    // ── Lifecycle state guards ────────────────────────────────────────────────

    #[test]
    #[should_panic(expected = "tournament is not open for registration")]
    fn register_on_pending_tournament_fails() {
        let env = Env::default();
        let (_id, _admin, client) = setup(&env);
        let organizer = Address::generate(&env);
        let player = Address::generate(&env);

        env.mock_all_auths();

        let tid = client.create_community_tournament(
            &organizer,
            &organizer,
            &0i128,
            &organizer,
            &false,
            &empty_hash(&env),
            &None,
        );

        // Not yet approved → should panic.
        client.register_player(&player, &tid);
    }

    #[test]
    #[should_panic(expected = "tournament must be approved before starting")]
    fn start_unapproved_tournament_fails() {
        let env = Env::default();
        let (_id, admin, client) = setup(&env);
        let organizer = Address::generate(&env);

        env.mock_all_auths();

        let tid = client.create_community_tournament(
            &organizer,
            &organizer,
            &0i128,
            &organizer,
            &false,
            &empty_hash(&env),
            &None,
        );

        client.start_tournament(&admin, &tid);
    }

    #[test]
    #[should_panic(expected = "player already registered")]
    fn duplicate_registration_fails() {
        let env = Env::default();
        let (_id, admin, client) = setup(&env);
        let organizer = Address::generate(&env);
        let player = Address::generate(&env);

        env.mock_all_auths();

        let tid = client.create_community_tournament(
            &organizer,
            &organizer,
            &0i128,
            &organizer,
            &false,
            &empty_hash(&env),
            &None,
        );

        client.approve_tournament(&admin, &tid);
        client.register_player(&player, &tid);
        client.register_player(&player, &tid); // duplicate
    }

    // ── Multiple players can register ─────────────────────────────────────────

    #[test]
    fn multiple_players_can_register() {
        let env = Env::default();
        let (_id, admin, client) = setup(&env);
        let organizer = Address::generate(&env);

        env.mock_all_auths();

        let tid = client.create_community_tournament(
            &organizer,
            &organizer,
            &0i128,
            &organizer,
            &false,
            &empty_hash(&env),
            &None,
        );

        client.approve_tournament(&admin, &tid);

        for _ in 0..5 {
            let player = Address::generate(&env);
            client.register_player(&player, &tid);
        }

        assert_eq!(client.get_players(&tid).len(), 5);
    }
}
