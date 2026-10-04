# Coding standards

Judgement calls a reviewer checks on a diff. clippy and rustfmt enforce the mechanical rules, and `AGENTS.md` names the architectural ones.

## Trait dispatch

A trait whose implementor devkit picks from config or detection is held as an enum that implements the trait through `ambassador`'s `#[delegate]`, not as `Box<dyn Trait>`. `vcs::Vcs` and the todo store's `Backend` are the pattern. A wrapper that forwards a trait's methods to a field, adding behaviour to some of them, delegates through `ambassador` too (`#[delegate(Trait, target = "field")]`) rather than restating each method.

A trait object stays where an enum would close a set that must stay open:

- tests inject a fake through it (`Tracker`, `Forge`), so a closed enum would have to carry the fake;
- its other implementors are stubs inside test modules (`PortProbe`, `ProcTable`);
- the trait belongs to another crate (taskchampion's `Server`).

`enum_dispatch` is for speed alone: a closed set of implementors on a path where a profile shows the dynamic call or the allocation.

Neither crate buys exhaustiveness, since a trait already makes every implementor supply every method. Introduce a trait for the shared behaviour of several types, never only to forward one method through a macro.
