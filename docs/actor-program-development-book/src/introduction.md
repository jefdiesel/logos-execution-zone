# Introduction

The Logos Execution Zone (LEZ) is a programmable chain with two kinds of accounts, public and private. A program is a RISC Zero guest binary, and the same bytecode serves both kinds. It runs on the sequencer when the account is public and inside the owner's prover when the account is private.

This book is the developer manual for the **actor model**: the program interface introduced by pull request 935, "transactional actors with side-effects". In this model a program is one function. It receives one message, sees the state of one actor, and answers with a new state and more messages. Everything else in the book follows from that.

## Why the actor model is worth a book

Three properties make designs possible that were awkward or impossible before.

1. **A program only reasons about itself.** A handler sees its own state on one account. It cannot read or write anything else. To affect another actor it sends a message, and the receiver decides what to do.
2. **A message can wait.** A `Cast` addressed to a stranger's private account is stored on chain like a letter in a mailbox. The owner collects it later, in their own transaction, with their own proof. A public program can pay a private person who is offline.
3. **A private actor cannot lie.** When a private actor sends a message, the proof covers the handler that produced it. A public actor of the same program can trust that message as a true statement about hidden state.

The catalog in Part IV is built on these three: a multisig whose members or vault are hidden, voting with secret voters, payment streams into private accounts, and a space game where ships are private and radio messages are casts.

## What this book is

1. **A manual.** Part I explains what a program author touches: accounts and actors, handlers, the two kinds of message, authorization, transactions, private accounts and their mailbox, deployment, the system programs and the toolchain.
2. **A map of the privacy model's edges.** Part II says what a private transaction publishes and lists what cannot be done privately. Read it before designing anything.
3. **A catalog of designs.** Parts III and IV turn the primitives into patterns and then into programs.

## How to read it

If you have never written an LEZ program, read Part I in order, then Part II, then pick the catalog entry closest to what you want to build.

If you know the model and want ideas, start at [How to read the catalog](catalog/index.md).

If you are checking a fact, the reference chapters list the guest API, the account-id derivations and the constants, with source locations.

## Conventions

- "LEE" is the Logos Execution Environment: the state machine, the program interface and the privacy circuit. "LEZ" is the zone built on it: the sequencer, the wallet, the builtin programs. Program authors mostly use types from the `lee_core` crate.
- **An actor** is a pair: an account and a program. "Alice's token actor" means the token program's state on Alice's account.
- Code in Part I and Part V is taken from the repository or matches it. Code in Part IV is illustrative. It follows the real API but has not been built or tested. Treat it as a design sketch.
- Amounts are `u128` unless a program says otherwise. Timestamps are Unix milliseconds.
- File paths are relative to the repository root.
- A box marked **Improvement over plan/apply** compares a point with the previous program model, whose manual is `docs/program-development-book/`. A box marked **Trade-off** says where the previous model was stronger.

## The state of the names

The book was written against the pull request at commit `faaa07251`. Review of that pull request agreed on several renames that had not landed when this was written. The book uses the names in the code.

| In the code and in this book | Agreed or proposed replacement |
|---|---|
| `Call`, `Cast` | `Sync…`, `Async…` message names |
| `define_actor_logic!` | `actor_entrypoint!` |
| `WholeTransaction` | `WholeExecution` |
| `Scope` | `Mode` |

If your checkout uses the new names, read `Call` as "synchronous message" and `Cast` as "asynchronous message".

## The one-paragraph model

An account is a nonce plus a map from program ids to byte strings. Each entry of that map is the state of one actor. A transaction delivers one root message to one actor. The program's handler runs with that actor's state and the message, and returns a new state, a list of Calls and a list of Casts. A Call is delivered at once, in the same transaction, and if anything fails the whole transaction fails. A Cast to an actor the transaction can run is also delivered at once. A Cast to anyone else is stored on chain for its recipient to collect. Handlers of public actors run on the sequencer. Handlers of private actors run on the owner's machine inside a proof, and the sequencer checks only the proof and the messages that crossed between the two sides.
