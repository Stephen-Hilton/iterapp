# Usecase Builder Agent

I'd like to create a new agent: usecase_builder
A use-case can be thought of as a high level E2E workflow, representing some descrete action taken by some actor within the codebase's scope.

For example, assume we have Netflix codebase, some use-cases might be:
- webpage user successful authentication with username/password
- webpage user successful authentication with oauth and google
- webpage user authentication failed
- logged in user shown their customized home page
- user clicks on a movie they want to watch, and movie starts
- user scrolls down to see extended options of shows to watch
- user clicks on "play games" and game page appears

Usecases should (typically) avoid technical instructions; i.e., almost all the above usecases will include some kind of authorization included, or include a service mesh mTLA between services, or include running containers, etc.  But those details don't matter to the user's experience, aka the usecase of simply logging in. 

## Usecase Goal
There are two main objectives to having usecases:
- Understanding all the ways a user (or agent) might traverse the technical environment.  The iterapp project is currently a hierarchy of largely technical c4 objects: 
  - context: data, communications, infrastructure, decision, etc. 
  - container: package for abstracted compute; docker container, library, OSS project, etc.
  - component: package for specific compute; library, package, crate, dll, etc.
  - code: today, we don't manage or visualize down to this level 
  A usecase will layer across the top of N-number of contexts, requiring containers and components to invisibly support. 
  Understanding what technical elements support which use-cases allows us to see what is heavily used, what sparsely used, and do ROI on features understanding how it might impact users.

- TDD, or test-driven development.  We want to guide development using usecases, to keep focus and complete the most important work first.  We use usecases as that guide for "most important" to keep our work user-centric.  By asking the iterloop to do testing by usecase, we can hammer out those high-use / highly visible usecases first, and expand carefully from there.

## Usecase Builder Actions
With the above context: the usecase builder agent will be handed a usecase idea from the user, and work thru the following steps:
- validate that the usecase can be solved by the current codebase; the usecase builder agent can FAIL the job if it determines:
  - the usecase isn't valid within the scope of the project, aka asking for a netflix usecase to "order food for delivery"
  - the usecase isn't clear in it's goals; aka "play movie" 
  - the usecase is overly technical; aka "run database query ABC"
  - the usecase is 
  If the usecase builder fails the workitem, that's fine, however make sure it spell out WHY it was rejected, and what the user might do to fix. Also suggest to the user they "create follow-up" to the original workitem, so context can be carried forward. 

- create the usecase object
  - create the usecase object
  - traverse all the m

## Usecase default location
There should be a iterapp project.setting for `default usecase path:` that defaults to `{codepath}/usecases/`
for projects like pdy-dev, I'm expecting this to end up at `~/dev/pdy-dev/usecases/`