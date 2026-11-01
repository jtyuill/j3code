//! Thread orchestration: the per-thread state machine ([`model`]), its
//! typed projection ([`projection`]), the omp session seam ([`session`]),
//! thread actors ([`actor`]) and the subscription streams ([`streams`]).

pub mod actor;
pub mod model;
pub mod projection;
pub mod session;
pub mod streams;
