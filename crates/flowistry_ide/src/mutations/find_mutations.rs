use flowistry::{infoflow::mutation::ModularMutationVisitor, mir::{aliases::Aliases, borrowck_facts::get_body_with_borrowck_facts}};
use log::debug;
use rustc_hir::def_id::DefId;
use rustc_middle::{
  mir::{visit::Visitor, Body, Location, Place},
  ty::TyCtxt,
};

pub fn find_mutations(
  tcx: TyCtxt<'tcx>,
  body: &Body<'tcx>,
  def_id: DefId,
  place: Place<'tcx>,
  aliases: &Aliases<'tcx>,
) -> Vec<Location> {
  let body_with_facts = get_body_with_borrowck_facts(tcx, def_id.as_local().unwrap());

  let mut locations = vec![];
  let reachable_values = aliases.reachable_values(tcx, body, def_id, place);
  debug!("reachable values: {reachable_values:?}");

  ModularMutationVisitor::new(
    tcx,
    body,
    def_id,
    |mutated_place, _, mutated_location, _| {
      debug!("checking mutated location {mutated_location:?}");

      let aliases = Aliases::build_loc(tcx, def_id, body_with_facts, mutated_location);
      let pointer_aliases = aliases.reachable_values(tcx, body, def_id, place);

      let mut place_conflicts = aliases.conflicts(mutated_place).to_owned();
      place_conflicts.intersect(&pointer_aliases);

      if place_conflicts.len() > 0 {
        debug!("  found conflicts: {place_conflicts:?}");
        locations.push(mutated_location);
      }
    },
  )
  .visit_body(body);

  return locations;
}
