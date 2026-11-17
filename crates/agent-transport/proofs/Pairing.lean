import Std

set_option warningAsError true

/-
Specification of transport::authorize. Identities and tokens are abstract natural
numbers; time uses only ordering, so these theorems include all u64 timestamps.
The functional maps model BTreeSet/BTreeMap lookup without their implementation.
Rust conformance is checked separately by PairingCases and pairing-conformance.

The owner must commit each successful update before the next attempt, as
host_runtime::pair_node does under its credentials lock. These proofs do not cover
locks, persistence failures, revocation, new invitations, or the transport itself.
-/
namespace Pairing

structure Trust where
  allowed : Nat → Bool
  invitations : Nat → Option Nat

inductive Outcome where
  | denied
  | existing
  | paired (next : Trust)

def authorize (trust : Trust) (node : Nat) (invitation : Option Nat) (now : Nat) : Outcome :=
  if trust.allowed node then .existing
  else match invitation with
    | none => .denied
    | some token => match trust.invitations token with
      | none => .denied
      | some expiry =>
        if now < expiry then
          .paired {
            allowed := fun peer => peer == node || trust.allowed peer
            invitations := fun candidate => if candidate == token then none else trust.invitations candidate
          }
        else .denied

def commit (trust : Trust) : Outcome → Trust
  | .paired next => next
  | _ => trust

theorem existing_peer_needs_no_invitation (trust : Trust) (node : Nat)
    (invitation : Option Nat) (now : Nat) (known : trust.allowed node = true) :
    authorize trust node invitation now = .existing := by
  simp [authorize, known]

theorem new_peer_needs_invitation (trust : Trust) (node now : Nat)
    (unknown : trust.allowed node = false) :
    authorize trust node none now = .denied := by
  simp [authorize, unknown]

theorem missing_invitation_is_rejected (trust : Trust) (node token now : Nat)
    (unknown : trust.allowed node = false) (missing : trust.invitations token = none) :
    authorize trust node (some token) now = .denied := by
  simp [authorize, unknown, missing]

theorem expired_invitation_is_rejected (trust : Trust) (node token now expiry : Nat)
    (unknown : trust.allowed node = false) (issued : trust.invitations token = some expiry)
    (expired : expiry ≤ now) :
    authorize trust node (some token) now = .denied := by
  simp [authorize, unknown, issued, Nat.not_lt.mpr expired]

theorem live_invitation_pairs_new_peer (trust : Trust) (node token now expiry : Nat)
    (unknown : trust.allowed node = false) (issued : trust.invitations token = some expiry)
    (live : now < expiry) :
    ∃ next, authorize trust node (some token) now = .paired next := by
  simp [authorize, unknown, issued, live]

theorem successful_pairing_requires_live_invitation (trust next : Trust)
    (node now : Nat) (invitation : Option Nat)
    (paired : authorize trust node invitation now = .paired next) :
    trust.allowed node = false ∧ ∃ token expiry,
      invitation = some token ∧ trust.invitations token = some expiry ∧ now < expiry := by
  unfold authorize at paired
  split at paired
  · contradiction
  · rename_i unknown
    cases invitation with
    | none => contradiction
    | some token =>
      cases issued : trust.invitations token with
      | none => simp [issued] at paired
      | some expiry =>
        simp only [issued] at paired
        split at paired
        · rename_i live
          exact ⟨Bool.eq_false_iff.mpr unknown, token, expiry, rfl, issued, live⟩
        · contradiction

theorem pairing_updates_only_peer_and_token (trust next : Trust) (node token now : Nat)
    (paired : authorize trust node (some token) now = .paired next) :
    next.allowed node = true ∧ next.invitations token = none ∧
      (∀ peer, peer ≠ node → next.allowed peer = trust.allowed peer) ∧
      (∀ other, other ≠ token → next.invitations other = trust.invitations other) := by
  unfold authorize at paired
  split at paired
  · contradiction
  · cases issued : trust.invitations token with
    | none => simp [issued] at paired
    | some expiry =>
      simp only [issued] at paired
      split at paired
      · cases paired
        refine ⟨by simp, by simp, ?_, ?_⟩
        · intro peer different
          simp [different]
        · intro other different
          simp [different]
      · contradiction

theorem consumed_invitation_cannot_pair_a_stranger (trust next : Trust)
    (node stranger token now later : Nat)
    (paired : authorize trust node (some token) now = .paired next)
    (different : stranger ≠ node) (unknown : trust.allowed stranger = false) :
    authorize next stranger (some token) later = .denied := by
  obtain ⟨_, consumed, unchanged, _⟩ := pairing_updates_only_peer_and_token trust next node token now paired
  exact missing_invitation_is_rejected next stranger token later
    ((unchanged stranger different).trans unknown) consumed

/- Issuing new invitations is deliberately outside this transition system. -/
theorem consumed_invitation_stays_absent (trust : Trust) (token node now : Nat)
    (invitation : Option Nat) (missing : trust.invitations token = none) :
    (commit trust (authorize trust node invitation now)).invitations token = none := by
  unfold authorize
  split
  · exact missing
  · cases invitation with
    | none => exact missing
    | some offered =>
      cases issued : trust.invitations offered with
      | none => simpa [issued, commit] using missing
      | some expiry =>
        simp only [issued]
        split
        · simp [commit, missing]
        · exact missing

theorem consumed_invitation_stays_absent_through_trace (trust : Trust) (token : Nat)
    (requests : List (Nat × Option Nat × Nat)) (missing : trust.invitations token = none) :
    (requests.foldl (fun state (node, invitation, now) =>
      commit state (authorize state node invitation now)) trust).invitations token = none := by
  induction requests generalizing trust with
  | nil => exact missing
  | cons request rest ih =>
    apply ih
    exact consumed_invitation_stays_absent trust token request.1 request.2.2 request.2.1 missing

#print axioms existing_peer_needs_no_invitation
#print axioms new_peer_needs_invitation
#print axioms missing_invitation_is_rejected
#print axioms expired_invitation_is_rejected
#print axioms live_invitation_pairs_new_peer
#print axioms successful_pairing_requires_live_invitation
#print axioms pairing_updates_only_peer_and_token
#print axioms consumed_invitation_cannot_pair_a_stranger
#print axioms consumed_invitation_stays_absent
#print axioms consumed_invitation_stays_absent_through_trace

end Pairing
