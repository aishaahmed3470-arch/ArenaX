/// Matchmaking End-to-End Integration Test Suite — Issue #1421
///
/// Verifies the complete pipeline:
///   queue → match → result (ELO update) → leaderboard rank refresh
///
/// Design principles:
/// - No external infrastructure: all state is in-process using Rust's standard
///   library data structures.  Redis and database are replaced by lightweight
///   in-memory fakes that mirror the service interfaces.
/// - Deterministic: fixed UUIDs, fixed ELO values, zero randomness.
/// - Fast: no I/O, no sleep, no timeouts.
///
/// The fakes deliberately duplicate only the behaviour tested here.
/// They are NOT general-purpose mocks; keep them minimal.

use chrono::Utc;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Domain types (mirrors backend/src/models/matchmaker.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum MatchStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone)]
struct QueueEntry {
    user_id: Uuid,
    game: String,
    game_mode: String,
    current_elo: i32,
    joined_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct Match {
    id: Uuid,
    player1_id: Uuid,
    player2_id: Uuid,
    player1_elo_before: i32,
    player2_elo_before: i32,
    winner_id: Option<Uuid>,
    status: MatchStatus,
    game_mode: String,
}

#[derive(Debug, Clone)]
struct EloRecord {
    user_id: Uuid,
    game: String,
    rating: i32,
    wins: i32,
    losses: i32,
}

#[derive(Debug, Clone)]
struct LeaderboardEntry {
    user_id: Uuid,
    rating: i32,
    /// 1-based rank (lower = better).
    rank: usize,
}

// ---------------------------------------------------------------------------
// ELO calculation (mirrors service/matchmaker.rs EloEngine)
// ---------------------------------------------------------------------------

const K_FACTOR: f64 = 32.0;

fn expected_score(own_elo: i32, opponent_elo: i32) -> f64 {
    1.0 / (1.0 + 10.0_f64.powf((opponent_elo - own_elo) as f64 / 400.0))
}

fn calculate_elo_changes(
    p1_elo: i32,
    p2_elo: i32,
    winner_id: Option<Uuid>,
    p1_id: Uuid,
    p2_id: Uuid,
) -> (i32, i32) {
    let e1 = expected_score(p1_elo, p2_elo);
    let e2 = 1.0 - e1;

    let (s1, s2) = match winner_id {
        Some(w) if w == p1_id => (1.0, 0.0),
        Some(w) if w == p2_id => (0.0, 1.0),
        _ => (0.5, 0.5), // draw
    };

    let new_p1 = p1_elo + (K_FACTOR * (s1 - e1)) as i32;
    let new_p2 = p2_elo + (K_FACTOR * (s2 - e2)) as i32;
    (new_p1, new_p2)
}

// ---------------------------------------------------------------------------
// In-memory store
// ---------------------------------------------------------------------------

#[derive(Default)]
struct InMemoryStore {
    /// Queued players: game_mode → Vec<QueueEntry>
    queue: HashMap<String, Vec<QueueEntry>>,
    /// Active/completed matches keyed by match ID
    matches: HashMap<Uuid, Match>,
    /// ELO records keyed by (user_id, game)
    elo: HashMap<(Uuid, String), EloRecord>,
}

impl InMemoryStore {
    fn new() -> Self {
        Self::default()
    }

    // ── Queue ────────────────────────────────────────────────────────────────

    fn enqueue(&mut self, entry: QueueEntry) {
        self.queue
            .entry(entry.game_mode.clone())
            .or_default()
            .push(entry);
    }

    fn dequeue_match_candidates(&mut self, game_mode: &str) -> Option<(QueueEntry, QueueEntry)> {
        let entries = self.queue.get_mut(game_mode)?;
        if entries.len() < 2 {
            return None;
        }
        // Pop two players in FIFO order (longest-waiting first).
        let p1 = entries.remove(0);
        let p2 = entries.remove(0);
        Some((p1, p2))
    }

    fn queue_len(&self, game_mode: &str) -> usize {
        self.queue.get(game_mode).map(|v| v.len()).unwrap_or(0)
    }

    // ── Matches ──────────────────────────────────────────────────────────────

    fn create_match(&mut self, p1: &QueueEntry, p2: &QueueEntry) -> Uuid {
        let id = Uuid::new_v4();
        self.matches.insert(
            id,
            Match {
                id,
                player1_id: p1.user_id,
                player2_id: p2.user_id,
                player1_elo_before: p1.current_elo,
                player2_elo_before: p2.current_elo,
                winner_id: None,
                status: MatchStatus::Pending,
                game_mode: p1.game_mode.clone(),
            },
        );
        id
    }

    fn set_match_in_progress(&mut self, match_id: Uuid) {
        if let Some(m) = self.matches.get_mut(&match_id) {
            m.status = MatchStatus::InProgress;
        }
    }

    fn complete_match(&mut self, match_id: Uuid, winner_id: Option<Uuid>) {
        if let Some(m) = self.matches.get_mut(&match_id) {
            m.winner_id = winner_id;
            m.status = MatchStatus::Completed;
        }
    }

    // ── ELO ─────────────────────────────────────────────────────────────────

    fn seed_elo(&mut self, user_id: Uuid, game: &str, rating: i32) {
        self.elo.insert(
            (user_id, game.to_string()),
            EloRecord { user_id, game: game.to_string(), rating, wins: 0, losses: 0 },
        );
    }

    fn get_elo(&self, user_id: Uuid, game: &str) -> Option<&EloRecord> {
        self.elo.get(&(user_id, game.to_string()))
    }

    fn apply_elo_update(&mut self, match_id: Uuid) {
        let m = self.matches.get(&match_id).expect("match not found").clone();
        let game = m.game_mode.clone();

        let (new_p1, new_p2) = calculate_elo_changes(
            m.player1_elo_before,
            m.player2_elo_before,
            m.winner_id,
            m.player1_id,
            m.player2_id,
        );

        if let Some(rec) = self.elo.get_mut(&(m.player1_id, game.clone())) {
            rec.rating = new_p1;
            match m.winner_id {
                Some(w) if w == m.player1_id => rec.wins += 1,
                _ => rec.losses += 1,
            }
        }

        if let Some(rec) = self.elo.get_mut(&(m.player2_id, game)) {
            rec.rating = new_p2;
            match m.winner_id {
                Some(w) if w == m.player2_id => rec.wins += 1,
                _ => rec.losses += 1,
            }
        }
    }

    // ── Leaderboard ──────────────────────────────────────────────────────────

    /// Build a leaderboard for `game`, sorted descending by rating.
    fn leaderboard(&self, game: &str) -> Vec<LeaderboardEntry> {
        let mut entries: Vec<LeaderboardEntry> = self
            .elo
            .values()
            .filter(|r| r.game == game)
            .map(|r| LeaderboardEntry { user_id: r.user_id, rating: r.rating, rank: 0 })
            .collect();

        entries.sort_by(|a, b| b.rating.cmp(&a.rating));

        for (i, entry) in entries.iter_mut().enumerate() {
            entry.rank = i + 1;
        }

        entries
    }

    fn rank_of(&self, user_id: Uuid, game: &str) -> Option<usize> {
        self.leaderboard(game)
            .iter()
            .find(|e| e.user_id == user_id)
            .map(|e| e.rank)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic UUIDs for repeatable assertions.
    fn player1_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap()
    }
    fn player2_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap()
    }
    fn player3_id() -> Uuid {
        Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()
    }

    const GAME: &str = "arena_fps";
    const GAME_MODE: &str = "ranked_1v1";

    /// Build a queue entry for a player.
    fn make_entry(user_id: Uuid, elo: i32) -> QueueEntry {
        QueueEntry {
            user_id,
            game: GAME.to_string(),
            game_mode: GAME_MODE.to_string(),
            current_elo: elo,
            joined_at: Utc::now(),
        }
    }

    // ── 1. Queue → Match ─────────────────────────────────────────────────────

    /// Two players with similar ELO are enqueued and matched.
    #[test]
    fn two_players_with_similar_elo_are_matched() {
        let mut store = InMemoryStore::new();

        store.seed_elo(player1_id(), GAME, 1200);
        store.seed_elo(player2_id(), GAME, 1220); // gap = 20 (well within 100)

        store.enqueue(make_entry(player1_id(), 1200));
        store.enqueue(make_entry(player2_id(), 1220));

        // Both players should be in the queue before matching.
        assert_eq!(store.queue_len(GAME_MODE), 2, "both players should be queued");

        let candidates = store.dequeue_match_candidates(GAME_MODE);
        assert!(candidates.is_some(), "matchmaker should find a pair");

        let (p1, p2) = candidates.unwrap();
        assert_eq!(p1.user_id, player1_id());
        assert_eq!(p2.user_id, player2_id());

        // ELO gap is 20 — well within the 100-point initial bucket.
        assert!((p1.current_elo - p2.current_elo).abs() <= 100);

        // Queue should now be empty.
        assert_eq!(store.queue_len(GAME_MODE), 0, "queue should be empty after matching");

        let match_id = store.create_match(&p1, &p2);
        let m = &store.matches[&match_id];

        assert_eq!(m.player1_id, player1_id());
        assert_eq!(m.player2_id, player2_id());
        assert_eq!(m.status, MatchStatus::Pending);
        assert_eq!(m.player1_elo_before, 1200);
        assert_eq!(m.player2_elo_before, 1220);
    }

    /// A single player in queue does not trigger a match.
    #[test]
    fn single_player_in_queue_is_not_matched() {
        let mut store = InMemoryStore::new();
        store.enqueue(make_entry(player1_id(), 1200));
        assert!(
            store.dequeue_match_candidates(GAME_MODE).is_none(),
            "one player should never produce a match"
        );
    }

    // ── 2. Match → Result → ELO update ───────────────────────────────────────

    /// After a result is reported, ELO ratings update correctly for both players.
    #[test]
    fn elo_updates_correctly_after_match_result() {
        let mut store = InMemoryStore::new();

        let p1_elo_before = 1200i32;
        let p2_elo_before = 1200i32;

        store.seed_elo(player1_id(), GAME, p1_elo_before);
        store.seed_elo(player2_id(), GAME, p2_elo_before);

        store.enqueue(make_entry(player1_id(), p1_elo_before));
        store.enqueue(make_entry(player2_id(), p2_elo_before));

        let (p1, p2) = store.dequeue_match_candidates(GAME_MODE).unwrap();
        let match_id = store.create_match(&p1, &p2);

        store.set_match_in_progress(match_id);
        // Player 1 wins.
        store.complete_match(match_id, Some(player1_id()));
        store.apply_elo_update(match_id);

        let p1_after = store.get_elo(player1_id(), GAME).unwrap();
        let p2_after = store.get_elo(player2_id(), GAME).unwrap();

        // When both players start at 1200 the expected score is 0.5 for each.
        // Winner gains K/2 = 16; loser loses 16.
        assert_eq!(p1_after.rating, 1200 + 16, "winner should gain 16 ELO");
        assert_eq!(p2_after.rating, 1200 - 16, "loser should lose 16 ELO");

        assert_eq!(p1_after.wins, 1);
        assert_eq!(p1_after.losses, 0);
        assert_eq!(p2_after.wins, 0);
        assert_eq!(p2_after.losses, 1);
    }

    /// A draw splits ELO 50/50 when players have equal ratings.
    #[test]
    fn equal_elo_draw_produces_no_rating_change() {
        let mut store = InMemoryStore::new();

        store.seed_elo(player1_id(), GAME, 1500);
        store.seed_elo(player2_id(), GAME, 1500);

        store.enqueue(make_entry(player1_id(), 1500));
        store.enqueue(make_entry(player2_id(), 1500));

        let (p1, p2) = store.dequeue_match_candidates(GAME_MODE).unwrap();
        let match_id = store.create_match(&p1, &p2);

        // Draw: winner_id = None
        store.complete_match(match_id, None);
        store.apply_elo_update(match_id);

        let p1_after = store.get_elo(player1_id(), GAME).unwrap();
        let p2_after = store.get_elo(player2_id(), GAME).unwrap();

        // Expected = 0.5, actual = 0.5 → delta = K * (0.5 - 0.5) = 0
        assert_eq!(p1_after.rating, 1500, "no rating change on equal draw");
        assert_eq!(p2_after.rating, 1500, "no rating change on equal draw");
    }

    /// An upset (underdog wins) produces a larger rating swing.
    #[test]
    fn underdog_win_produces_larger_elo_gain() {
        let mut store = InMemoryStore::new();

        let underdog_elo = 1000i32;
        let favourite_elo = 1400i32;

        store.seed_elo(player1_id(), GAME, underdog_elo);
        store.seed_elo(player2_id(), GAME, favourite_elo);

        store.enqueue(make_entry(player1_id(), underdog_elo));
        store.enqueue(make_entry(player2_id(), favourite_elo));

        let (p1, p2) = store.dequeue_match_candidates(GAME_MODE).unwrap();
        let match_id = store.create_match(&p1, &p2);

        // Underdog wins.
        store.complete_match(match_id, Some(player1_id()));
        store.apply_elo_update(match_id);

        let (new_u, new_f) = calculate_elo_changes(
            underdog_elo,
            favourite_elo,
            Some(player1_id()),
            player1_id(),
            player2_id(),
        );

        let p1_after = store.get_elo(player1_id(), GAME).unwrap();
        let p2_after = store.get_elo(player2_id(), GAME).unwrap();

        assert_eq!(p1_after.rating, new_u);
        assert_eq!(p2_after.rating, new_f);

        // Underdog gains more than K/2 (> 16 points).
        assert!(
            p1_after.rating - underdog_elo > 16,
            "underdog win should produce a larger-than-average ELO gain"
        );
        // Favourite loses more than K/2.
        assert!(
            favourite_elo - p2_after.rating > 16,
            "favourite loss should produce a larger-than-average ELO drop"
        );
    }

    // ── 3. ELO update → Leaderboard refresh ──────────────────────────────────

    /// Leaderboard ranks update immediately after ELO changes are applied.
    #[test]
    fn leaderboard_rank_updates_immediately_after_result() {
        let mut store = InMemoryStore::new();

        // Three players: P2 starts highest, then P1, then P3.
        store.seed_elo(player1_id(), GAME, 1200);
        store.seed_elo(player2_id(), GAME, 1300);
        store.seed_elo(player3_id(), GAME, 1100);

        // Initial leaderboard: P2(1) > P1(2) > P3(3)
        assert_eq!(store.rank_of(player2_id(), GAME), Some(1));
        assert_eq!(store.rank_of(player1_id(), GAME), Some(2));
        assert_eq!(store.rank_of(player3_id(), GAME), Some(3));

        // P1 vs P2 match — P1 (underdog) wins.
        store.enqueue(make_entry(player1_id(), 1200));
        store.enqueue(make_entry(player2_id(), 1300));

        let (p1, p2) = store.dequeue_match_candidates(GAME_MODE).unwrap();
        let match_id = store.create_match(&p1, &p2);
        store.complete_match(match_id, Some(player1_id()));
        store.apply_elo_update(match_id);

        let p1_new = store.get_elo(player1_id(), GAME).unwrap().rating;
        let p2_new = store.get_elo(player2_id(), GAME).unwrap().rating;

        // After upset win, P1's rating should exceed P2's.
        assert!(
            p1_new > p2_new,
            "player1 ({p1_new}) should surpass player2 ({p2_new}) after upset"
        );

        // Leaderboard must now reflect the updated standings instantly.
        let rank_p1 = store.rank_of(player1_id(), GAME).unwrap();
        let rank_p2 = store.rank_of(player2_id(), GAME).unwrap();
        let rank_p3 = store.rank_of(player3_id(), GAME).unwrap();

        assert_eq!(rank_p1, 1, "player1 should now be rank 1 after upset win");
        assert!(rank_p2 > rank_p1, "player2 should drop below player1");
        assert_eq!(rank_p3, 3, "player3 (uninvolved) should remain rank 3");
    }

    /// Leaderboard is stable (no change) for a player not involved in any match.
    #[test]
    fn uninvolved_player_rank_unchanged_after_other_match() {
        let mut store = InMemoryStore::new();

        store.seed_elo(player1_id(), GAME, 1200);
        store.seed_elo(player2_id(), GAME, 1220);
        store.seed_elo(player3_id(), GAME, 800); // far below both

        let rank_before = store.rank_of(player3_id(), GAME).unwrap();

        store.enqueue(make_entry(player1_id(), 1200));
        store.enqueue(make_entry(player2_id(), 1220));

        let (p1, p2) = store.dequeue_match_candidates(GAME_MODE).unwrap();
        let match_id = store.create_match(&p1, &p2);
        store.complete_match(match_id, Some(player1_id()));
        store.apply_elo_update(match_id);

        let rank_after = store.rank_of(player3_id(), GAME).unwrap();

        assert_eq!(rank_before, rank_after, "player3's rank should not change");
    }

    // ── 4. Full pipeline — queue → match → result → leaderboard ─────────────

    /// End-to-end: enqueue two players, match them, report result, verify the
    /// complete downstream chain updates correctly.
    #[test]
    fn full_pipeline_queue_match_result_leaderboard() {
        let mut store = InMemoryStore::new();

        let p1_start = 1150;
        let p2_start = 1180;

        store.seed_elo(player1_id(), GAME, p1_start);
        store.seed_elo(player2_id(), GAME, p2_start);

        // --- Step 1: Enqueue ---
        store.enqueue(make_entry(player1_id(), p1_start));
        store.enqueue(make_entry(player2_id(), p2_start));

        assert_eq!(store.queue_len(GAME_MODE), 2, "step 1: both players queued");

        // --- Step 2: Match ---
        let (p1, p2) = store
            .dequeue_match_candidates(GAME_MODE)
            .expect("step 2: should find a match");

        let match_id = store.create_match(&p1, &p2);
        store.set_match_in_progress(match_id);

        assert_eq!(store.queue_len(GAME_MODE), 0, "step 2: queue empty after match");
        assert_eq!(
            store.matches[&match_id].status,
            MatchStatus::InProgress,
            "step 2: match in progress"
        );

        // --- Step 3: Result ---
        store.complete_match(match_id, Some(player2_id())); // P2 wins
        store.apply_elo_update(match_id);

        assert_eq!(
            store.matches[&match_id].status,
            MatchStatus::Completed,
            "step 3: match completed"
        );

        let p1_elo = store.get_elo(player1_id(), GAME).unwrap();
        let p2_elo = store.get_elo(player2_id(), GAME).unwrap();

        assert!(
            p1_elo.rating < p1_start,
            "step 3: loser ELO should decrease"
        );
        assert!(
            p2_elo.rating > p2_start,
            "step 3: winner ELO should increase"
        );

        // ELO is zero-sum for equal stakes.
        assert_eq!(
            p1_elo.rating + p2_elo.rating,
            p1_start + p2_start,
            "step 3: total ELO is conserved"
        );

        // --- Step 4: Leaderboard ---
        let rank_p1 = store.rank_of(player1_id(), GAME).unwrap();
        let rank_p2 = store.rank_of(player2_id(), GAME).unwrap();

        assert_eq!(rank_p2, 1, "step 4: winner should be rank 1");
        assert_eq!(rank_p1, 2, "step 4: loser should be rank 2");
    }

    // ── 5. ELO calculation unit tests ────────────────────────────────────────

    #[test]
    fn expected_score_at_equal_ratings_is_half() {
        let e = expected_score(1200, 1200);
        assert!((e - 0.5).abs() < 1e-9, "expected score should be 0.5 at equal ratings");
    }

    #[test]
    fn expected_score_higher_rated_player_is_above_half() {
        let e = expected_score(1400, 1200);
        assert!(e > 0.5, "higher-rated player should have > 0.5 expected score");
    }

    #[test]
    fn elo_changes_sum_to_zero_for_decisive_result() {
        let (new1, new2) = calculate_elo_changes(
            1200,
            1300,
            Some(player1_id()),
            player1_id(),
            player2_id(),
        );
        assert_eq!(
            new1 + new2,
            1200 + 1300,
            "ELO should be conserved: gains equal losses"
        );
    }

    #[test]
    fn elo_changes_sum_to_zero_for_draw() {
        let (new1, new2) =
            calculate_elo_changes(1000, 2000, None, player1_id(), player2_id());
        assert_eq!(
            new1 + new2,
            1000 + 2000,
            "ELO should be conserved: gains equal losses in draw"
        );
    }

    // ── 6. Queue ordering ────────────────────────────────────────────────────

    /// Players are dequeued FIFO — longest-waiting player is matched first.
    #[test]
    fn queue_is_fifo_longest_waiting_player_matched_first() {
        let mut store = InMemoryStore::new();

        store.seed_elo(player1_id(), GAME, 1200);
        store.seed_elo(player2_id(), GAME, 1200);
        store.seed_elo(player3_id(), GAME, 1200);

        // Enqueue P1 first, then P2, then P3.
        store.enqueue(make_entry(player1_id(), 1200));
        store.enqueue(make_entry(player2_id(), 1200));
        store.enqueue(make_entry(player3_id(), 1200));

        let (first_out, second_out) = store.dequeue_match_candidates(GAME_MODE).unwrap();
        assert_eq!(
            first_out.user_id,
            player1_id(),
            "first enqueued player should be dequeued first"
        );
        assert_eq!(
            second_out.user_id,
            player2_id(),
            "second enqueued player should be dequeued second"
        );

        // P3 remains in the queue.
        assert_eq!(store.queue_len(GAME_MODE), 1, "one player should remain");
    }

    // ── 7. Multiple sequential matches ───────────────────────────────────────

    /// Running three back-to-back matches adjusts ratings cumulatively.
    #[test]
    fn multiple_sequential_matches_accumulate_elo() {
        let mut store = InMemoryStore::new();

        store.seed_elo(player1_id(), GAME, 1200);
        store.seed_elo(player2_id(), GAME, 1200);

        let mut p1_wins = 0;
        let mut p2_wins = 0;

        // Run 3 matches; P1 wins the first two, P2 wins the third.
        let outcomes = [
            Some(player1_id()),
            Some(player1_id()),
            Some(player2_id()),
        ];

        for winner in outcomes {
            let p1_elo = store.get_elo(player1_id(), GAME).unwrap().rating;
            let p2_elo = store.get_elo(player2_id(), GAME).unwrap().rating;

            store.enqueue(make_entry(player1_id(), p1_elo));
            store.enqueue(make_entry(player2_id(), p2_elo));

            let (p1, p2) = store.dequeue_match_candidates(GAME_MODE).unwrap();
            let match_id = store.create_match(&p1, &p2);
            store.complete_match(match_id, winner);
            store.apply_elo_update(match_id);

            match winner {
                Some(w) if w == player1_id() => p1_wins += 1,
                Some(_) => p2_wins += 1,
                None => {}
            }
        }

        let p1_final = store.get_elo(player1_id(), GAME).unwrap();
        let p2_final = store.get_elo(player2_id(), GAME).unwrap();

        assert_eq!(p1_final.wins, p1_wins);
        assert_eq!(p2_final.wins, p2_wins);

        // P1 won more; should have higher rating.
        assert!(
            p1_final.rating > p2_final.rating,
            "p1 ({}) should be rated higher than p2 ({}) after 2-1 record",
            p1_final.rating,
            p2_final.rating
        );
    }
}
