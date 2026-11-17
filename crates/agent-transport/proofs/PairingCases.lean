import Pairing
import Lean

set_option warningAsError true

open Pairing

/- Finite bridge to Rust, not an extraction or proof of the Rust implementation.
   All subsets of three peers, two invitations independently absent or expiring
   at 0, 1, or u64::MAX, and missing/unknown/present tokens are exercised. Each
   first request is followed by same-time replays from every peer, then an
   invitation-free reconnect at u64::MAX. Full states are compared after every
   step, including rejection and existing-peer outcomes. -/
structure Snapshot where
  allowed : List Nat
  invitations : List (Nat × Nat)
  deriving Lean.ToJson

structure Request where
  node : Nat
  invitation : Option Nat
  now : Nat
  deriving Lean.ToJson

structure Step where
  request : Request
  outcome : String
  state : Snapshot
  deriving Lean.ToJson

structure Trace where
  initial : Snapshot
  steps : List Step
  deriving Lean.ToJson

def Snapshot.trust (snapshot : Snapshot) : Trust where
  allowed := snapshot.allowed.contains
  invitations := fun token => (snapshot.invitations.find? (fun entry => entry.1 == token)).map Prod.snd

def snapshot (trust : Trust) : Snapshot where
  allowed := (List.range 3).filter trust.allowed
  invitations := (List.range 2).filterMap fun token => (trust.invitations token).map (token, ·)

def makeStep (trust : Trust) (request : Request) : Trust × Step :=
  let result := authorize trust request.node request.invitation request.now
  let next := commit trust result
  let outcome := match result with
    | .denied => "denied"
    | .existing => "existing"
    | .paired _ => "paired"
  (next, { request, outcome, state := snapshot next })

def maxTime : Nat := 18446744073709551615

def main : IO Unit := do
  let stdout ← IO.getStdout
  for mask in List.range 8 do
    for firstExpiry in [none, some 0, some 1, some maxTime] do
      for secondExpiry in [none, some 0, some 1, some maxTime] do
        let initial : Snapshot := {
          allowed := (List.range 3).filter fun peer => (mask / 2 ^ peer) % 2 == 1
          invitations := [(0, firstExpiry), (1, secondExpiry)].filterMap fun (token, expiry) =>
            expiry.map (token, ·)
        }
        for node in List.range 3 do
          for invitation in [none, some 0, some 1, some 2] do
            for now in [0, 1, 2, maxTime - 1, maxTime] do
              let (afterFirst, first) := makeStep initial.trust { node, invitation, now }
              let mut trust := afterFirst
              let mut steps := [first]
              for peer in List.range 3 do
                let (next, step) := makeStep trust { node := peer, invitation, now }
                trust := next
                steps := steps ++ [step]
              let (_, reconnect) := makeStep trust { node, invitation := none, now := maxTime }
              let trace : Trace := { initial, steps := steps ++ [reconnect] }
              stdout.putStrLn (Lean.toJson trace).compress
