
after looking at `iter markers` and *.iter.md files, I think we need a major overhaul to make our *.iter.md files more consistent.
This will be a wide-ranging change; 


- we need to add a missing attributes in our *.iter.md files:
  - type: [c4object | interface | usecase | testgroup | etc.]  aka what kind of object data is this?
    - I'm surprised to see this missing; I thought we were doing explicit typing, rather than extrapolating type via...?
    - we should update the engine / webapp / API / CLI / etc. to expose and use this

- add several new filtering CLI / API options to: `iter markers --project .` 
  - filter_type:   [marker | testgroup | bizreq | techreq | interface | etc.]  limits json output to a paricular type
  - filter_ancestors_of: `~/some/code/path/some_file.iter.md` returns all ancestor nodes of the supplied file (by reference, not directory)
  - filter_descendants_of: `~/some/code/path/some_file.iter.md` returns all descendant nodes of the supplied file (by reference, not directory)
  
The use-case we're working towards is more explicit testgroup/test directory relationship; today there isn't an enforced directory relationship between test00.sh and the codepath where that test should run, causing tests to start in unexpected locations
Targeted Workflow:
- automated testloop starts, grabs and reads the testgroups, finds one to run: `RUNTESTGROUP="folder/path/testgroup.iter.md"`
- testgroup runs `PARENTMARKER=${iter markers --filter_type marker --filter_ancestors_of $RUNTESTGROUP}[0]` (psuedo-code, not sure syntax is correct) 
- PARENTMARKER = marker.iter.md file that is the most direct parent; aka the .marker.iter.md that refers to this
- PARENTMARKERFO = extracting from the json object PARENTMARKER['path'] and throwing away the filename itself, leaving the parent folder only
- when starting the `test/test00.sh` we pass in `$PARENTMARKERFO` as the default configpath (unless set to something specific); i.e., it always