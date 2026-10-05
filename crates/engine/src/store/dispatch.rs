//! Round-robin selection policy. Channels remain owned by the database worker;
//! only accepting a job advances the turn. Empty/closed queues cost no turn.
use super::db::Class;

#[derive(Clone, Copy, Debug, Default)]
pub struct Dispatch {
    next: usize,
}
impl Dispatch {
    pub fn order(self) -> [Class; 3] {
        std::array::from_fn(|offset| Class::ALL[(self.next + offset) % 3])
    }
    pub fn served(&mut self, class: Class) {
        self.next = (class.index() + 1) % 3;
    }
}
