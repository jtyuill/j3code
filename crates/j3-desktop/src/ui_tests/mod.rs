//! UI integration tests: the production app against an in-process j3 server with a
//! scripted omp, driven through Rune's input (pointer at element centres, keys, typed
//! text) and checked on element keys, accessibility labels, app state and what the fake
//! omp was asked. See [`harness`].

mod composer;
mod harness;
mod omp;
mod shell;
mod sidebar;
mod surfaces;
mod thread;
