use flowistry::source_map::{find_bodies, Range};
use serde::Serialize;

use crate::plugin::{FlowistryError, FlowistryResult};

#[derive(Serialize)]
pub struct SpansOutput {
  spans: Vec<Range>,
}

struct Callbacks {
  filename: String,
  output: Option<FlowistryResult<SpansOutput>>,
}

impl rustc_driver::Callbacks for Callbacks {
  fn after_parsing<'tcx>(
    &mut self,
    compiler: &rustc_interface::interface::Compiler,
    queries: &'tcx rustc_interface::Queries<'tcx>,
  ) -> rustc_driver::Compilation {
    queries.global_ctxt().unwrap().take().enter(|tcx| {
      let spans = find_bodies(tcx).into_iter().map(|(span, _)| span);

      self.output = Some((|| {
        let source_map = compiler.session().source_map();
        let source_file = Range {
          byte_start: 0,
          byte_end: 0,
          char_start: 0,
          char_end: 0,
          filename: self.filename.clone(),
        }
        .source_file(source_map)
        .map_err(|_| FlowistryError::FileNotFound)?;

        let spans = spans
          .into_iter()
          .filter(|span| {
            source_map.lookup_source_file(span.lo()).name_hash == source_file.name_hash
          })
          .filter_map(|span| Range::from_span(span, source_map).ok())
          .collect::<Vec<_>>();
        Ok(SpansOutput { spans })
      })());
    });
    rustc_driver::Compilation::Stop
  }
}

pub fn spans(args: &[String], filename: String) -> FlowistryResult<SpansOutput> {
  let mut callbacks = Callbacks {
    filename,
    output: None,
  };
  crate::plugin::run_with_callbacks(args, &mut callbacks)?;
  callbacks.output.unwrap()
}
