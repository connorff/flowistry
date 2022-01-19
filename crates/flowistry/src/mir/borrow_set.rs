use std::{
  collections::{HashMap, HashSet},
  ops::Index,
};

use log::debug;
use rustc_hir::Mutability;
use rustc_borrowck::borrow_set::{BorrowData, LocalsStateAtExit, TwoPhaseActivation};
use rustc_data_structures::fx::FxIndexMap;
use rustc_index::bit_set::BitSet;
use rustc_middle::{
  mir::{
    self, traversal,
    visit::{NonUseContext, PlaceContext, Visitor, MutatingUseContext},
    Body, Local, Location, Place, ProjectionElem,
  },
  ty::{self, RegionKind, RegionVid, TyCtxt},
};
use rustc_mir_dataflow::move_paths::MoveData;

trait BorrowSetPlaceExt<'tcx> {
  fn ignore_borrow(
    &self,
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    locals_state_at_exit: &LocalsStateAtExit,
  ) -> bool;
}

impl BorrowSetPlaceExt<'tcx> for Place<'tcx> {
  fn ignore_borrow(
    &self,
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    locals_state_at_exit: &LocalsStateAtExit,
  ) -> bool {
    // If a local variable is immutable, then we only need to track borrows to guard
    // against two kinds of errors:
    // * The variable being dropped while still borrowed (e.g., because the fn returns
    //   a reference to a local variable)
    // * The variable being moved while still borrowed
    //
    // In particular, the variable cannot be mutated -- the "access checks" will fail --
    // so we don't have to worry about mutation while borrowed.
    if let LocalsStateAtExit::SomeAreInvalidated {
      has_storage_dead_or_moved,
    } = locals_state_at_exit
    {
      let ignore = !has_storage_dead_or_moved.contains(self.local)
        && body.local_decls[self.local].mutability == Mutability::Not;
      debug!("ignore_borrow: local {:?} => {:?}", self.local, ignore);
      if ignore {
        return true;
      }
    }

    for (i, elem) in self.projection.iter().enumerate() {
      let proj_base = &self.projection[.. i];

      if elem == ProjectionElem::Deref {
        let ty = Place::ty_from(self.local, proj_base, body, tcx).ty;
        match ty.kind() {
          ty::Ref(_, _, Mutability::Not) if i == 0 => {
            // For references to thread-local statics, we do need
            // to track the borrow.
            if body.local_decls[self.local].is_ref_to_thread_local() {
              continue;
            }
            return true;
          }
          ty::RawPtr(..) | ty::Ref(_, _, Mutability::Not) => {
            // For both derefs of raw pointers and `&T`
            // references, the original path is `Copy` and
            // therefore not significant.  In particular,
            // there is nothing the user can do to the
            // original path that would invalidate the
            // newly created reference -- and if there
            // were, then the user could have copied the
            // original path into a new variable and
            // borrowed *that* one, leaving the original
            // path unborrowed.
            return true;
          }
          _ => {}
        }
      }
    }

    false
  }
}

fn build_locals_state_at_exit(
  locals_are_invalidated_at_exit: bool,
  body: &Body<'tcx>,
  move_data: &MoveData<'tcx>,
) -> LocalsStateAtExit {
  struct HasStorageDead(BitSet<Local>);

  impl<'tcx> Visitor<'tcx> for HasStorageDead {
    fn visit_local(&mut self, local: &Local, ctx: PlaceContext, _: Location) {
      if ctx == PlaceContext::NonUse(NonUseContext::StorageDead) {
        self.0.insert(*local);
      }
    }
  }

  if locals_are_invalidated_at_exit {
    LocalsStateAtExit::AllAreInvalidated
  } else {
    let mut has_storage_dead = HasStorageDead(BitSet::new_empty(body.local_decls.len()));
    has_storage_dead.visit_body(&body);
    let mut has_storage_dead_or_moved = has_storage_dead.0;
    for move_out in &move_data.moves {
      if let Some(index) = move_data.base_local(move_out.path) {
        has_storage_dead_or_moved.insert(index);
      }
    }
    LocalsStateAtExit::SomeAreInvalidated {
      has_storage_dead_or_moved,
    }
  }
}

pub struct BorrowSet<'tcx> {
  /// The fundamental map relating bitvector indexes to the borrows
  /// in the MIR. Each borrow is also uniquely identified in the MIR
  /// by the `Location` of the assignment statement in which it
  /// appears on the right hand side. Thus the location is the map
  /// key, and its position in the map corresponds to `BorrowIndex`.
  pub location_map: FxIndexMap<Location, BorrowData<'tcx>>,

  /// Locations which activate borrows.
  /// NOTE: a given location may activate more than one borrow in the future
  /// when more general two-phase borrow support is introduced, but for now we
  /// only need to store one borrow index.
  pub activation_map: HashMap<Location, Vec<BorrowIndex>>,

  /// Map from local to all the borrows on that local.
  pub local_map: HashMap<mir::Local, HashSet<BorrowIndex>>,
}

impl<'tcx> Index<BorrowIndex> for BorrowSet<'tcx> {
  type Output = BorrowData<'tcx>;

  fn index(&self, index: BorrowIndex) -> &BorrowData<'tcx> {
    &self.location_map[index.as_usize()]
  }
}

impl<'tcx> BorrowSet<'tcx> {
  pub fn build(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    locals_are_invalidated_at_exit: bool,
    move_data: &MoveData<'tcx>,
  ) -> Self {
    let mut visitor = GatherBorrowsRustc {
      tcx,
      body: &body,
      location_map: Default::default(),
      activation_map: Default::default(),
      local_map: Default::default(),
      pending_activations: Default::default(),
      locals_state_at_exit: build_locals_state_at_exit(
        locals_are_invalidated_at_exit,
        body,
        move_data,
      ),
    };

    for (block, block_data) in traversal::preorder(&body) {
      visitor.visit_basic_block_data(block, block_data);
    }

    BorrowSet {
      location_map: visitor.location_map,
      activation_map: visitor.activation_map,
      local_map: visitor.local_map,
    }
  }
}

rustc_index::newtype_index! {
  pub struct BorrowIndex {
      DEBUG_FORMAT = "bw{}"
  }
}

fn to_region_vid(region: RegionKind) -> RegionVid {
  if let ty::ReVar(vid) = region {
    vid
  } else {
    unreachable!()
  }
}

struct GatherBorrowsRustc<'a, 'tcx> {
  tcx: TyCtxt<'tcx>,
  body: &'a Body<'tcx>,
  location_map: FxIndexMap<Location, BorrowData<'tcx>>,
  activation_map: HashMap<Location, Vec<BorrowIndex>>,
  local_map: HashMap<mir::Local, HashSet<BorrowIndex>>,

  /// When we encounter a 2-phase borrow statement, it will always
  /// be assigning into a temporary TEMP:
  ///
  ///    TEMP = &foo
  ///
  /// We add TEMP into this map with `b`, where `b` is the index of
  /// the borrow. When we find a later use of this activation, we
  /// remove from the map (and add to the "tombstone" set below).
  pending_activations: HashMap<mir::Local, BorrowIndex>,

  locals_state_at_exit: LocalsStateAtExit,
}

impl<'a, 'tcx> Visitor<'tcx> for GatherBorrowsRustc<'a, 'tcx> {
  fn visit_assign(
    &mut self,
    assigned_place: &mir::Place<'tcx>,
    rvalue: &mir::Rvalue<'tcx>,
    location: mir::Location,
  ) {
    if let mir::Rvalue::Ref(region, kind, ref borrowed_place) = *rvalue {
      if borrowed_place.ignore_borrow(self.tcx, self.body, &self.locals_state_at_exit) {
        debug!("ignoring_borrow of {:?}", borrowed_place);
        return;
      }

      let region = to_region_vid(region.clone());

      let borrow = BorrowData {
        kind,
        region,
        reserve_location: location,
        activation_location: TwoPhaseActivation::NotTwoPhase,
        borrowed_place: *borrowed_place,
        assigned_place: *assigned_place,
      };
      let (idx, _) = self.location_map.insert_full(location, borrow);
      let idx = BorrowIndex::from(idx);

      self.insert_as_pending_if_two_phase(location, assigned_place, kind, idx);

      self
        .local_map
        .entry(borrowed_place.local)
        .or_default()
        .insert(idx);
    }

    self.super_assign(assigned_place, rvalue, location)
  }

  fn visit_local(&mut self, temp: &Local, context: PlaceContext, location: Location) {
    if !context.is_use() {
      return;
    }

    // We found a use of some temporary TMP
    // check whether we (earlier) saw a 2-phase borrow like
    //
    //     TMP = &mut place
    if let Some(&borrow_index) = self.pending_activations.get(temp) {
      let borrow_data = &mut self.location_map[borrow_index.as_usize()];

      // Watch out: the use of TMP in the borrow itself
      // doesn't count as an activation. =)
      if borrow_data.reserve_location == location
        && context == PlaceContext::MutatingUse(MutatingUseContext::Store)
      {
        return;
      }

      if let TwoPhaseActivation::ActivatedAt(other_location) =
        borrow_data.activation_location
      {
        debug!(
          "found two uses for 2-phase borrow temporary {:?}: \
                     {:?} and {:?}",
          temp, location, other_location,
        );
      }

      // Otherwise, this is the unique later use that we expect.
      // Double check: This borrow is indeed a two-phase borrow (that is,
      // we are 'transitioning' from `NotActivated` to `ActivatedAt`) and
      // we've not found any other activations (checked above).
      assert_eq!(
        borrow_data.activation_location,
        TwoPhaseActivation::NotActivated,
        "never found an activation for this borrow!",
      );
      self
        .activation_map
        .entry(location)
        .or_default()
        .push(borrow_index);

      borrow_data.activation_location = TwoPhaseActivation::ActivatedAt(location);
    }
  }

  fn visit_rvalue(&mut self, rvalue: &mir::Rvalue<'tcx>, location: mir::Location) {
    if let mir::Rvalue::Ref(region, kind, ref place) = *rvalue {
      // double-check that we already registered a BorrowData for this

      let borrow_data = &self.location_map[&location];
      assert_eq!(borrow_data.reserve_location, location);
      assert_eq!(borrow_data.kind, kind);
      assert_eq!(borrow_data.region, to_region_vid(region.clone()));
      assert_eq!(borrow_data.borrowed_place, *place);
    }

    self.super_rvalue(rvalue, location)
  }
}

impl<'a, 'tcx> GatherBorrowsRustc<'a, 'tcx> {
  /// If this is a two-phase borrow, then we will record it
  /// as "pending" until we find the activating use.
  fn insert_as_pending_if_two_phase(
    &mut self,
    start_location: Location,
    assigned_place: &mir::Place<'tcx>,
    kind: mir::BorrowKind,
    borrow_index: BorrowIndex,
  ) {
    debug!(
      "Borrows::insert_as_pending_if_two_phase({:?}, {:?}, {:?})",
      start_location, assigned_place, borrow_index,
    );

    if !kind.allows_two_phase_borrow() {
      debug!("  -> {:?}", start_location);
      return;
    }

    // When we encounter a 2-phase borrow statement, it will always
    // be assigning into a temporary TEMP:
    //
    //    TEMP = &foo
    //
    // so extract `temp`.
    let temp = assigned_place.as_local().unwrap();

    // Consider the borrow not activated to start. When we find an activation, we'll update
    // this field.
    {
      let borrow_data = &mut self.location_map[borrow_index.as_usize()];
      borrow_data.activation_location = TwoPhaseActivation::NotActivated;
    }

    // Insert `temp` into the list of pending activations. From
    // now on, we'll be on the lookout for a use of it. Note that
    // we are guaranteed that this use will come after the
    // assignment.
    let old_value = self.pending_activations.insert(temp, borrow_index);
    if let Some(old_index) = old_value {
      debug!(
        "found already pending activation for temp: {:?} \
                       at borrow_index: {:?} with associated data {:?}",
        temp,
        old_index,
        self.location_map[old_index.as_usize()]
      );
    }
  }
}
