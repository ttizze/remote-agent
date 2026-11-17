import Lean

set_option warningAsError true

namespace Persistence

def offer (current candidate : Nat) : Option Nat :=
  if current < candidate then some candidate else none

theorem offered_build_is_newer {current candidate build : Nat}
    (selected : offer current candidate = some build) : current < build := by
  unfold offer at selected
  split at selected <;> simp_all

def candidate (base extra : Nat) (draft matching uploaded : Bool) :=
  if !draft && matching && uploaded then max base extra else base

theorem draft_cannot_displace (base extra : Nat) (matching uploaded : Bool) :
    candidate base extra true matching uploaded = base := by simp [candidate]

def acceptKeys (keys : List String) : Bool := decide keys.Nodup

theorem duplicate_rejected (key : String) (rest : List String) (present : key ∈ rest) :
    acceptKeys (key :: rest) = false := by simp [acceptKeys, List.nodup_cons, present]

structure CacheDecision where
  loaded : Bool
  retained : Bool
  deriving Lean.ToJson

def cache (current sameScope sameThread : Bool) : CacheDecision :=
  ⟨current && sameScope && sameThread, current && (!sameScope || sameThread)⟩

theorem foreign_copy_preserved (sameThread : Bool) :
    cache true false sameThread = ⟨false, true⟩ := by simp [cache]

theorem loaded_copy_retained (current sameScope sameThread : Bool)
    (loaded : (cache current sameScope sameThread).loaded = true) :
    (cache current sameScope sameThread).retained = true := by
  cases current <;> cases sameScope <;> cases sameThread <;> simp_all [cache]

def emit (kind : String) (fields : List (String × Lean.Json)) : IO Unit :=
  IO.println (Lean.Json.mkObj (("kind", Lean.toJson kind) :: fields)).compress

def bools := [false, true]

end Persistence

open Persistence Lean

def main : IO Unit := do
  for extra in [4, 6] do
    for current in [4, 5, 6] do
      for draft in bools do
        for matching in bools do
          for uploaded in bools do
            emit "updates" [("extra", toJson extra), ("current", toJson current),
              ("draft", toJson draft), ("matching", toJson matching), ("uploaded", toJson uploaded),
              ("expected", toJson (offer current (candidate 5 extra draft matching uploaded)))]
  for keys in [[], [""], ["a", "b"], ["b", "a"], ["", ""], ["a", "b", "a"]] do
    emit "drafts" [("keys", toJson keys), ("accepted", toJson (acceptKeys keys))]
  for current in bools do
    for sameScope in bools do
      for sameThread in bools do
        emit "cache" [("current", toJson current), ("same_scope", toJson sameScope),
          ("same_thread", toJson sameThread), ("expected", toJson (cache current sameScope sameThread))]
