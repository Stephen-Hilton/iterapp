Looking at workitem: 71123487-907f-4c3d-8340-1e79cf5f4329                                                                                                                         
it states:                                                                                                                                                                        
```                                                                                                                                                                               
`reqs/techreq.md:120` (PDY-TECH-030) enumerates exactly four marker files that belong in a component root: bizreq, techreq, interface, and testgroups. It does not mention        
`<component>.iter.md`. The mainwork told me to create that fifth file, and I did, in all 16 component roots plus 5 more at container and project level. So the platform-wide      
requirement and the tree now disagree: an agent following PDY-TECH-030 would not know the structure node is expected, and a future ingest pass could quietly omit it.             
                                                                                                                                                                                
Nothing in either file contradicts the interface ids, kinds or endpoints I assigned, so the committed work stands. But there is a loose end worth your attention:                 
`reqs/interfaces.md` defines per-interface ids of the form `<component>-IF-###`, where one component owns four or five of them, while the frontmatter `interface:` key holds a    
single kebab-case id per marker file covering all of that component's interfaces. They don't map one-to-one, and neither document mentions the other. Whether the frontmatter ids 
get recorded beside the `-IF-###` entries, derived from them, or kept as a separate scanner-only namespace is a decision I should not make unilaterally.                          
```                                                                                                                                                                               
                                                                                                                                                                                
two items:                                                                                                                                                                        
                                                                                                                                                                                
- "It does not mention `<component>.iter.md`" seems like a problem; do we not have a description/example of the marker.iter.md file?  The agents being created from .iter should  
have default context about it's own *.iter.md file structure types, maybe even an engine level "validate" function that can be called, to deterministically validate the format   
(and maybe some lightweight fixing).  Thoughts?                                                                                                                                   
                                                                                                                                                                                
- similar feedback for interfaces.iter.md;  I always envisioned interfaces as the data contract between two compute units; aka, compute A provides {"some":["data"]} and compute  
B accepts {"some":["data"]}.  Looking at the comment above, as well as the "Interfaces" section of the iterapp.projects screen, I'm not seeing contracts.  I'm expecting json or  
xml or a list of values or objects; instead I'm seeing records where the "contract" is a prompt-like block of text?  I'm expecting something like psuedo-json, and getting 2      
pages of text instead.  Where is the disconnect?   