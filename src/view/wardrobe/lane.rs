//! The Wardrobe's work queue: one lane per account holds the last confirmed look and the changes still on their way, in
//! the order they were asked for. What the page shows is the confirmed look with every waiting change applied on top, so
//! a click answers at once and a refused change simply drops out of the lane.

use std::sync::Arc;

use image::RgbaImage;
use super::desk::{Look, Owns};
use crate::core::skin_change::Model;
use crate::core::wardrobe::Step;
use crate::ui::forms::Problem;

/// One thing the lane does for an account.
#[derive(Clone)]
pub enum Op {
    Refresh,
    Skin { file: String, png: Arc<Vec<u8>>, image: Arc<RgbaImage>, model: Model },
    Cape(Option<String>),
}

impl Op {
    fn slot(&self) -> Option<u8> {
        match self {
            Self::Refresh => None,
            Self::Skin { .. } => Some(0),
            Self::Cape(_) => Some(1),
        }
    }

    /// What a card of the page is called while this change is on its way.
    pub fn key(&self) -> String {
        match self {
            Self::Refresh => "refresh".into(),
            Self::Skin { file, .. } => format!("skin:{file}"),
            Self::Cape(id) => format!("cape:{}", id.as_deref().unwrap_or("")),
        }
    }
}

/// A queued change and how far it is.
#[derive(Clone)]
pub struct Job {
    pub id: u64,
    pub op: Op,
    pub running: bool,
    pub step: Option<Step>,
}

/// Where a card's change stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    Waiting,
    Working(Option<Step>),
}

/// A change the service refused, kept until the person asks again.
#[derive(Clone, PartialEq)]
pub struct Failure {
    pub key: String,
    pub problem: Problem,
    pub signin: bool,
}

/// What one account has: its confirmed look, the work in line and what went wrong last.
#[derive(Clone, Default)]
pub struct Lane {
    pub look: Option<Look>,
    pub jobs: Vec<Job>,
    pub failure: Option<Failure>,
    pub load: Option<(Problem, bool)>,
}

fn bare(microsoft: bool) -> Look {
    Look { skin: None, model: Model::Classic, worn: None, capes: Vec::<Owns>::new(), cape: None, microsoft, known: false, fetched: 0 }
}

fn apply(look: &mut Look, op: &Op) {
    match op {
        Op::Refresh => {}
        Op::Skin { file, image, model, .. } => {
            look.worn = Some(file.clone());
            look.model = *model;
            look.skin = Some(image.clone());
        }
        Op::Cape(id) => look.cape.clone_from(id),
    }
}

impl Lane {
    /// Queues a change of skin or cape behind the ones in line. A change that has not started and covers the same slot is
    /// replaced, a refresh waits behind the changes the person asked for, and only one refresh is ever in line.
    pub fn push(&mut self, id: u64, op: Op) {
        let slot = op.slot();
        if slot.is_some() {
            self.failure = None;
            self.jobs.retain(|j| j.running || j.op.slot() != slot);
            if self.jobs.iter().rev().find(|j| j.op.slot() == slot).is_some_and(|j| j.op.key() == op.key()) {
                return;
            }
        } else {
            self.load = None;
            if self.jobs.iter().any(|j| j.op.slot().is_none()) {
                return;
            }
        }
        let at = match slot {
            Some(_) => self.jobs.iter().position(|j| !j.running && j.op.slot().is_none()).unwrap_or(self.jobs.len()),
            None => self.jobs.len(),
        };
        self.jobs.insert(at, Job { id, op, running: false, step: None });
    }

    /// The change to start now, when none runs.
    pub fn next(&mut self) -> Option<Job> {
        if self.jobs.iter().any(|j| j.running) {
            return None;
        }
        let job = self.jobs.first_mut()?;
        job.running = true;
        Some(job.clone())
    }

    pub fn step(&mut self, id: u64, step: Step) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.step = Some(step);
        }
    }

    pub fn finish(&mut self, id: u64) -> Option<Job> {
        let at = self.jobs.iter().position(|j| j.id == id)?;
        Some(self.jobs.remove(at))
    }

    /// Takes a change that went through into the confirmed look, for when the service did not tell the result.
    pub fn bake(&mut self, op: &Op, microsoft: bool) {
        apply(self.look.get_or_insert_with(|| bare(microsoft)), op);
    }

    /// The look to show: the confirmed one with every change in line applied; none while nothing is known.
    pub fn shown(&self, microsoft: bool) -> Option<Look> {
        let asked = self.jobs.iter().any(|j| j.op.slot().is_some());
        if self.look.is_none() && !asked {
            return None;
        }
        let mut look = self.look.clone().unwrap_or_else(|| bare(microsoft));
        for job in &self.jobs {
            apply(&mut look, &job.op);
        }
        Some(look)
    }

    /// Whether the skin `file` is waiting to be put on or being put on.
    pub fn skin_pending(&self, file: &str) -> Option<Pending> {
        self.pending(|op| matches!(op, Op::Skin { file: f, .. } if f == file))
    }

    pub fn cape_pending(&self, id: Option<&str>) -> Option<Pending> {
        self.pending(|op| matches!(op, Op::Cape(c) if c.as_deref() == id))
    }

    fn pending(&self, is: impl Fn(&Op) -> bool) -> Option<Pending> {
        self.jobs.iter().find(|j| is(&j.op)).map(|j| if j.running { Pending::Working(j.step) } else { Pending::Waiting })
    }

    /// The step of the change running now, and how many wait behind it.
    pub fn working(&self) -> Option<(Option<Step>, usize)> {
        let running = self.jobs.iter().find(|j| j.running)?;
        Some((running.step, self.jobs.len() - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skin(file: &str) -> Op {
        Op::Skin { file: file.into(), png: Arc::new(Vec::new()), image: Arc::new(RgbaImage::new(1, 1)), model: Model::Classic }
    }

    fn files(lane: &Lane) -> Vec<String> {
        lane.jobs.iter().map(|j| j.op.key()).collect()
    }

    fn look(worn: &str) -> Look {
        Look { worn: Some(worn.into()), microsoft: true, known: true, ..bare(true) }
    }

    #[test]
    fn changes_run_in_the_order_they_were_asked() {
        let mut lane = Lane::default();
        lane.push(1, Op::Cape(Some("a".into())));
        lane.push(2, skin("x"));
        assert_eq!(files(&lane), ["cape:a", "skin:x"]);
        assert_eq!(lane.next().map(|j| j.id), Some(1));
        assert!(lane.next().is_none());
        lane.finish(1);
        assert_eq!(lane.next().map(|j| j.id), Some(2));
    }

    #[test]
    fn a_waiting_change_of_the_same_slot_is_replaced_and_a_running_one_is_not() {
        let mut lane = Lane::default();
        lane.push(1, skin("a"));
        lane.next();
        lane.push(2, skin("b"));
        lane.push(3, skin("c"));
        assert_eq!(files(&lane), ["skin:a", "skin:c"]);
        lane.push(4, Op::Cape(None));
        assert_eq!(files(&lane), ["skin:a", "skin:c", "cape:"]);
    }

    #[test]
    fn asking_for_what_is_already_on_its_way_adds_nothing() {
        let mut lane = Lane::default();
        lane.push(1, skin("a"));
        lane.next();
        lane.push(2, skin("a"));
        assert_eq!(lane.jobs.len(), 1);
        lane.push(3, skin("b"));
        lane.push(4, skin("a"));
        assert_eq!(files(&lane), ["skin:a"]);
    }

    #[test]
    fn a_refresh_waits_behind_what_the_person_asked_and_is_not_doubled() {
        let mut lane = Lane::default();
        lane.push(1, Op::Refresh);
        lane.next();
        lane.push(2, Op::Refresh);
        assert_eq!(lane.jobs.len(), 1);
        lane.push(3, skin("a"));
        assert_eq!(files(&lane), ["refresh", "skin:a"]);
        lane.finish(1);
        lane.push(4, Op::Refresh);
        lane.push(5, skin("b"));
        assert_eq!(files(&lane), ["skin:b", "refresh"]);
    }

    #[test]
    fn the_page_shows_the_confirmed_look_with_the_changes_in_line_on_top() {
        let mut lane = Lane { look: Some(look("old")), ..Lane::default() };
        assert_eq!(lane.shown(true).and_then(|l| l.worn), Some("old".into()));
        lane.push(1, skin("new"));
        lane.push(2, Op::Cape(Some("c".into())));
        let shown = lane.shown(true).unwrap();
        assert_eq!((shown.worn.as_deref(), shown.cape.as_deref()), (Some("new"), Some("c")));
        lane.finish(1);
        assert_eq!(lane.shown(true).and_then(|l| l.worn), Some("old".into()));
    }

    #[test]
    fn nothing_is_shown_until_something_is_known_or_asked() {
        let mut lane = Lane::default();
        assert!(lane.shown(true).is_none());
        lane.push(1, Op::Refresh);
        assert!(lane.shown(true).is_none());
        lane.push(2, skin("a"));
        assert_eq!(lane.shown(true).and_then(|l| l.worn), Some("a".into()));
    }

    #[test]
    fn asking_again_clears_the_last_refusal() {
        let mut lane = Lane::default();
        lane.failure = Some(Failure { key: "skin:a".into(), problem: Problem::plain("no"), signin: false });
        lane.push(1, Op::Refresh);
        assert!(lane.failure.is_some());
        lane.push(2, skin("a"));
        assert!(lane.failure.is_none());
    }

    #[test]
    fn cards_know_whether_their_change_waits_or_runs() {
        let mut lane = Lane::default();
        lane.push(1, skin("a"));
        lane.push(2, Op::Cape(None));
        assert_eq!(lane.skin_pending("a"), Some(Pending::Waiting));
        lane.next();
        lane.step(1, Step::Upload);
        assert_eq!(lane.skin_pending("a"), Some(Pending::Working(Some(Step::Upload))));
        assert_eq!(lane.cape_pending(None), Some(Pending::Waiting));
        assert_eq!(lane.skin_pending("b"), None);
        assert_eq!(lane.working(), Some((Some(Step::Upload), 1)));
    }

    #[test]
    fn a_change_the_service_did_not_confirm_can_be_taken_over() {
        let mut lane = Lane::default();
        lane.bake(&Op::Cape(Some("c".into())), true);
        assert_eq!(lane.look.and_then(|l| l.cape), Some("c".into()));
    }
}
