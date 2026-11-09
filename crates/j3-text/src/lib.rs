//! Chat text preparation with no UI types, shared by the j3 apps: TeX math in
//! markdown ([`math`]), omp's magic keywords ([`keywords`]) and `^model`
//! mentions ([`mention`]), and a sent user message's displayed form
//! ([`message`]).

pub mod keywords;
pub mod math;
pub mod mention;
pub mod message;
