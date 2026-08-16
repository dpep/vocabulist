# Engineering Notes

## Release Notes

Version 4.2 focuses on stability rather than new capability. We spent the cycle chasing down slow queries and quietly won back about a third of the latency we had lost since the spring.

The scheduler no longer retries a failed job immediately. It now waits with an increasing delay between attempts, which cut our alert volume roughly in half during the first week alone.

We fixed a long-standing issue where canceled requests kept their memory allocated until the next garbage collection pass. Under sustained load this showed up as a slow, steady climb in memory that eventually forced a restart.

The connection pool now sheds idle connections after five minutes of inactivity instead of holding them indefinitely. This matters most for the reporting service, which used to leave hundreds of open connections sitting around doing nothing.

Authentication tokens issued before this release remain valid until they expire naturally. There is no need to force a fresh login for existing users, though new tokens carry a shorter lifetime by default.

We removed the old export tool that wrote comma separated files directly to local disk. Almost nobody used it, and the few who did have already moved to the newer streaming export, which handles larger data sets without running out of memory.

Compression on the message queue is now enabled by default. Message size drops by roughly sixty percent for typical payloads, at the cost of a small amount of additional processor time on both ends.

A subtle rounding error in the billing calculation has been corrected. Affected accounts were undercharged by a few cents per invoice; we are not attempting to collect the difference retroactively, since the amounts involved are trivial and the customer confusion would not be worth it.

The administrative panel now shows queue depth as a live number instead of refreshing once a minute. Several people on the operations team asked for this after the outage in June, when stale numbers on the dashboard sent everyone chasing the wrong problem.

Deployment now waits for a successful health check before shifting traffic to the new instances. Previously we shifted traffic immediately and let failures surface as customer complaints, which was, in hindsight, not a great way to find out something was broken.

The search service now supports partial word matching by default, which several customers requested after noticing that a single missing letter would return no results at all.

Error messages returned by the account service now include a specific reason rather than a generic failure code. Support has already reported spending less time asking customers to reproduce problems that the message now explains directly.

We tightened the rate limit on the public reporting service after a client with a broken setting managed to overwhelm it during a busy afternoon. Legitimate usage should be unaffected, since the new limit sits well above anything we have observed from normal traffic.

Time zone handling in the scheduling service has been rewritten from scratch. The previous implementation assumed a fixed offset and quietly produced wrong results twice a year whenever clocks changed.

Notifications sent by the billing service now arrive within a minute of the triggering event instead of up to an hour later. The delay was caused by a batching step that made sense years ago but had long outlived its original purpose.

The mobile client now retries failed uploads automatically in the background instead of showing an error and expecting the user to try again manually. Early feedback suggests this alone accounts for a noticeable drop in support tickets about lost photos.

Search result ranking now takes recency into account alongside relevance, so a very recent match no longer gets buried beneath older, more frequently viewed ones. Feedback so far has been positive, though we are watching closely for any unintended side effects on less common searches.

The password reset process no longer reveals whether an email address is registered, closing a minor information leak that a security researcher flagged a few months ago. The user experience is nearly identical; only the wording of the confirmation message changed.

## Design Rationale

We chose to divide the primary database by account rather than by geography. The geographic approach looked appealing at first because it matches how our customers are actually distributed, but it falls apart the moment a single large customer operates across several regions at once.

Dividing by account keeps every query for a given customer on one machine. That single property eliminates an entire category of cross machine joins we would otherwise need to write, test, and maintain forever.

The obvious downside is that our largest accounts can outgrow a single machine faster than smaller ones. We accepted that tradeoff deliberately: it is a problem we can solve later for a handful of customers, rather than a problem we would face immediately for everyone.

We considered building our own message queue before settling on an existing one. Writing a basic queue is not hard. Writing one that survives a crash without losing messages, replays correctly after a restart, and behaves predictably when consumers fall behind took the existing project the better part of a decade to get right, and we did not want to spend that decade ourselves.

For the caching layer we picked a write through approach over write behind. Write behind gives better write latency, which sounds attractive until you consider what happens when the cache and the database disagree after a crash. We would rather pay a small latency cost on every write than debug a data mismatch at three in the morning.

The decision to keep settings in plain text files, rather than a database table, was mostly about operational simplicity. A text file can be reviewed, compared, and rolled back using the same tools we already use for source code. A database row cannot be reviewed by a second engineer before it takes effect, and that review step has caught real mistakes more than once.

We debated whether to validate input at the edge of the system or deep inside each service. Edge validation is faster to implement and catches the obvious mistakes early, but it tends to drift out of sync with the actual rules each service enforces. We settled on validating at both layers: cheap checks at the edge to reject garbage quickly, and thorough checks inside each service as the actual source of truth.

Retries with an increasing delay were chosen over a fixed delay because a fixed delay tends to produce synchronized bursts of traffic after a widespread failure. Every client wakes up and retries at exactly the same moment, which can turn a brief outage into a second, self-inflicted one. Adding a small random offset to each delay spreads that traffic out and avoids the pileup entirely.

We looked seriously at giving every team its own separate database before deciding against it. Separate databases would have made scaling each team's data easier, but at the cost of duplicating account information across every one of them, and keeping duplicated data consistent has historically been where our worst bugs come from.

Choosing between synchronous and asynchronous processing for the notification service came down to a simple question: does the caller need to know the outcome before continuing? For most notifications the answer is no, so we process them in the background and let the caller move on immediately.

We evaluated three different approaches to handling partial failures in a batch operation before picking the simplest one: process each item independently, record which ones failed, and let the caller retry just those. It is not the most elegant design, but it is the easiest one to reason about at two in the morning.

The choice to store audit records in an append only table, rather than updating a single row per entity, was driven entirely by the need to reconstruct history later. An update in place destroys the very information an auditor eventually needs to see.

We chose to keep the search index and the primary database as separate systems rather than folding search into the database itself. Combining them would have simplified operations somewhat, but every database engine we evaluated made a real tradeoff in search quality to get there, and search quality matters enormously to how customers experience the product.

Choosing a single large table over several smaller, specialized ones for event storage came down to query flexibility. Analysts frequently ask questions we did not anticipate when the design was chosen, and a single wide table lets them explore without waiting on an engineer to add a new table for every new question.

We rejected the idea of building a custom authentication system from scratch, even though our requirements are fairly simple. Authentication is exactly the kind of problem where a small oversight has serious consequences, and building it ourselves would mean carrying that risk indefinitely instead of relying on years of scrutiny an established approach has already received.

## Debugging Narrative

The report started simple: search results were occasionally missing entries that clearly existed in the database. Not consistently, not for every user, and not reproducible on demand, which is the worst kind of bug to receive a report about.

I started by comparing the indexed count against the actual row count for a handful of affected accounts. The numbers were close but not equal, off by a small number of records each time, which ruled out anything catastrophic and pointed toward a timing problem instead.

The indexing process reads from the database in batches and writes to the search index afterward. My first guess was a batch boundary issue, where a record updated between two batches might get skipped entirely. I added logging around every batch and waited for the problem to happen again.

It took three days to catch it in the log output. When it finally showed up, the missing record had been updated by a background job at almost the exact same moment the indexing batch containing it was being read. The read saw the old version, the write happened a moment later, and nothing ever went back to pick up the change.

Once I understood the shape of the problem, the fix was straightforward: record a marker before each batch begins, and after the batch completes, check whether anything changed in that range since the marker was set. If so, schedule that range again for another pass.

Testing this properly meant deliberately creating the exact race condition rather than hoping to stumble onto it. I wrote a test that pauses the indexing process midway through a batch, updates one of the records it just read, then resumes and asserts the record eventually appears correctly in the index.

The fix has been running for two weeks without a single missing record reported, which is either a genuine resolution or an extraordinary coincidence. I'm inclined to believe the former, but I left the extra logging in place for another month just to be sure.

The most useful lesson from this one: intermittent bugs that only show up under real traffic are almost always timing problems. Reaching for more logging before reaching for a debugger saved a great deal of time here, because the bug simply would not reproduce under a debugger's slower pace.

Before settling on the timing explanation, I ruled out a simpler one first: that the search index itself was simply behind the database by design. It was not; the two are supposed to stay within a few seconds of each other, confirmed by checking the lag directly.

I also considered whether the missing records shared some property, like a particular account or a particular type of update, that might point to a narrower cause. They did not. The only thing they had in common was proximity in time to an unrelated write, which is exactly what pointed me toward a race condition in the first place.

One thing that made this investigation slower than it needed to be: the existing logging recorded when a batch started but not which specific records it contained. Adding that detail turned out to be the single most useful change I made during the entire investigation.

Reproducing the issue locally eventually required simulating real traffic patterns rather than simple, isolated test calls. A single request never triggered the problem; only a steady stream of overlapping requests hitting the same records did.

I nearly gave up on this one after the first week, convinced it might be a hardware issue on one particular machine rather than anything in the code. Swapping the machine changed nothing, which at least ruled that theory out and pointed me back toward the application itself.

Writing down a clear, falsifiable hypothesis before each experiment made a real difference here. Without one, it is easy to convince yourself that ambiguous results confirm whatever theory you already favor.

## Code Review Comments

This function handles three unrelated concerns: parsing the request, validating the account, and writing to the audit log. Splitting it into three smaller functions would make each one easier to test in isolation, and the audit logging in particular deserves its own focused test.

Nice catch moving the expensive lookup outside the loop. That alone should cut the processing time for large batches substantially, since we were repeating the same query for every single record before.

This comparison assumes the list is already sorted, but nothing above guarantees that. Either sort explicitly before this point, or add a comment explaining why the caller is trusted to hand us sorted input.

I would avoid catching the general exception here. It hides real failures alongside the one specific case you actually want to handle, and someone debugging a future problem will waste time ruling out this handler before finding the actual cause.

Good test coverage on the success path, but I do not see a case for what happens when the remote service times out. That is the failure mode we actually hit most often in production, so it deserves its own test rather than an assumption that it behaves like any other error.

This constant is defined in four different files with four slightly different values. Before merging, can we pick one source of truth and have the others reference it? Otherwise someone will update one copy during a future change and miss the rest.

The variable naming here is a little confusing since both variables represent counts, but at different stages of the calculation. Renaming one to something like adjusted count would save the next reader a moment of confusion.

Small thing, but the comment above this block describes what the code used to do before your change, not what it does now. Worth updating so it does not mislead the next person who reads it.

This looks correct, but I would rather see the retry limit as a named constant than a bare number buried in the middle of the function. Six months from now nobody will remember why it is specifically five.

Approving with one suggestion: consider adding a brief comment explaining why we skip the validation step for internal requests. It is not obvious from the code alone, and I had to ask you directly to understand the reasoning.

The naming here is inconsistent with the rest of the module; everywhere else we call this a request, but here it is called a message. Small thing, but consistency makes the code easier to search.

This test passes today only because the two operations happen to run in the same order every time. Nothing in the code actually guarantees that order, so the test should either enforce it explicitly or cover the case where it does not hold.

Really like how this handles the empty input case explicitly instead of letting it fall through to the general path. Made the logic much easier to follow on a first read.

This function is now over two hundred lines long and handles at least four distinct responsibilities. I would not block on it, but a follow up ticket to split this apart would save whoever touches it next a fair amount of frustration.

This error message is technically accurate but will not mean anything to whoever eventually reads it in a log at two in the morning. Consider including the actual value that failed validation, not just the fact that validation failed.

I appreciate that you added a test for the boundary condition, but the assertion only checks that no exception was thrown. Can we also assert on the returned value, since a silently wrong result would pass this test just as easily as a correct one?

This change touches a shared utility used by several other services. Worth confirming with the owners of those services before merging, just in case any of them depend on the exact behavior you are changing here.

## Incident Postmortem

At approximately two in the afternoon, error rates on the checkout service climbed sharply within the span of about ninety seconds. Customers attempting to complete a purchase received a generic failure message, and the on call engineer was paged within three minutes of the first alert.

The immediate cause was a database migration that ran during a maintenance window earlier that morning. The migration added a column with a default value, which required rewriting every existing row in a table with several hundred million entries. That rewrite held a lock far longer than anyone anticipated.

The lock itself did not cause the outage directly. The outage happened because a separate, unrelated deployment went out shortly afterward and attempted to acquire the same lock while establishing its connections. That deployment then queued behind the migration, and every request depending on it queued behind the deployment in turn.

Recovery began once the on call engineer identified the blocked migration and canceled it manually. Traffic returned to normal within about four minutes of the cancellation, and the queued requests drained shortly after.

Total customer impact lasted twenty six minutes. During that window, roughly eight percent of checkout attempts failed outright, and a further share succeeded only after a noticeable delay.

Several factors made this harder to diagnose than it should have been. The migration and the deployment were scheduled independently by two different teams, and neither system had visibility into what the other was doing. There was no single dashboard showing active locks alongside pending deployments, so the connection between the two had to be pieced together after the fact from separate logs.

We are making three changes as a direct result of this incident. First, migrations that touch large tables will run in smaller pieces rather than as a single operation, so no individual step can hold a lock for more than a few seconds. Second, deployments will check for an active migration before proceeding and wait rather than compete for the same resource. Third, we are building a single view that shows both migrations and deployments together, so the next person investigating a similar issue does not have to reconstruct the timeline by hand.

None of these changes are difficult individually. What made this incident possible was the gap between two systems that each behaved reasonably in isolation but interacted badly in combination. That's a pattern worth watching for elsewhere, not just here.

It is worth noting that automated monitoring did detect the elevated error rate almost immediately. The delay in this incident came entirely from diagnosis, not detection, which suggests our alerting is working as intended even though the underlying cause took longer to find.

Customer support received around forty inquiries during the incident window, a small fraction of the total affected users. Most customers appear to have simply retried their purchase once the system recovered, without ever contacting anyone.

We considered whether to notify customers proactively during the incident and decided against it, given how quickly the issue resolved once identified. In hindsight that call was reasonable, though we agreed to revisit the threshold for proactive notification as part of the broader review.

The team that owns the checkout service has requested a dedicated environment for testing large migrations before they run against production data. That request predates this incident but had not been prioritized; it is being prioritized now.

We reviewed whether existing monitoring would have caught the interaction between the migration and the deployment ahead of time, and concluded it would not have, since neither system was designed to be aware of the other. Building that awareness is a larger project than anything we can commit to immediately.

A number of people from outside the immediate team contributed to the investigation once it was clear the cause was not obvious, and that extra set of eyes shortened the time to resolution considerably. We are grateful for the quick response, even on short notice.

## New Hire Documentation

Welcome to the team. This document covers the basics you need to get your development environment running and to make your first small change with confidence.

Start by setting up the database locally rather than pointing at a shared environment. A local copy is slower to prepare but means your experiments cannot affect anyone else, and you can reset it freely whenever something goes wrong.

The message queue runs locally as well during development. You do not need to set up anything special; the default settings are meant to work out of the box for a fresh setup, and if they do not, that is worth flagging immediately rather than working around quietly.

Most of the actual business logic lives in the service layer, one folder per major area of responsibility. Reading through the account service first is a reasonable place to start, since nearly everything else in the system eventually depends on it in some way.

Tests live alongside the code they exercise rather than in a separate tree. This was a deliberate choice made years ago to keep the test for a given piece of behavior physically close to that behavior, so a change and its corresponding test are easy to find together.

Before you open your first pull request, read through a handful of recently merged ones to get a feel for the level of detail expected in a description. We generally prefer a short explanation of why a change was made over a lengthy description of exactly what changed, since the difference itself already shows the what.

Every service reports metrics automatically once it starts, so you should not need to add anything manual for basic visibility. If you find yourself wanting a metric that does not already exist, ask in the team channel first, since there is a decent chance something similar already exists under a name you have not thought to search for.

Deployment happens automatically once a change merges to the main branch and passes every required check. There is no manual approval step beyond code review, which means review is where quality actually gets enforced, so take it seriously in both directions.

If something breaks and you are not sure who owns it, the ownership file at the root of the project lists a team for nearly every directory. When in doubt, ask in the general channel rather than guessing; nobody minds a question, and a wrong guess acted upon can cost far more time than the question would have.

Take your time during the first couple of weeks. Everyone remembers their own early period being longer and more confusing than they expected, and asking questions early is consistently faster than trying to figure everything out alone.

Communication mostly happens in a handful of team channels rather than in private messages. Keeping discussion visible means anyone can search past conversations instead of asking the same question that was already answered a month earlier.

Documentation drifts out of date the moment nobody is responsible for it, so we try to keep it as close as possible to the thing it describes. When you notice something wrong while reading, the fastest fix is usually to correct it yourself rather than filing a note for someone else to handle eventually.

Meetings are kept intentionally short and infrequent, since most decisions here happen through written discussion instead of live conversation. If you find yourself needing a meeting to resolve something, it is worth asking first whether a written question would get you the same answer faster.

Your manager will schedule a short check in during your second week, mostly to see how things are going and to answer anything you have been hesitant to ask in a larger group. There are no wrong questions during that conversation.

Access to production systems is granted gradually rather than all at once. You will start with read access to logs and metrics, and broader access follows naturally as you take on responsibilities that require it.

The team maintains a short list of common mistakes new engineers make during their first month, mostly around assumptions that hold in the local environment but not in production. Reading through it now will save you from repeating a few of them.

Pairing with someone more experienced for your first few changes is strongly encouraged, even for something that looks simple. A second set of eyes catches assumptions you would not otherwise notice you were making.

## Status Updates

Morning update: the migration finished overnight without incident. Row counts match between the old and new tables, so I am moving on to updating the application to read from the new location today.

Quick heads up, I'm seeing elevated error rates on the search service starting about ten minutes ago. Looking into it now, will report back shortly once I know more.

Update on the search issue: traced it to a settings change that went out an hour earlier. Reverting now, should be resolved within a few minutes.

Confirmed resolved. Error rates are back to baseline and have stayed there for the last fifteen minutes. Writing up a short summary of what happened before I forget the details.

Heads up that I will be out most of tomorrow. If the deployment for the billing change needs attention while I am away, the details are in the pull request description, and either of you should be able to handle it without me.

Finished the review on the pagination change, left a couple of small comments but nothing that should block merging. Nice work getting the edge cases right, especially the empty result case, which is easy to overlook.

Reminder that the staging environment will be unavailable for about an hour this afternoon while we apply the security patch. Plan any testing that depends on it accordingly.

Good news: the memory leak we've been chasing for two weeks is fixed. Turned out to be an old subscription that was never being cleaned up when a connection closed. Small fix, embarrassingly long time to find it.

Shipping the retry logic change now. Watching the error dashboard closely for the next half hour before calling it done, just in case something unexpected shows up under real traffic that didn't show up in testing.

End of week summary: three bugs fixed, one small feature shipped, and the ongoing database migration is about sixty percent complete. On track to finish early next week barring any surprises.

Heads up, the deployment tool is behaving strangely this morning; several people have reported it hanging partway through. Avoid deploying until we sort this out, should have an update within the hour.

Deployment tool issue traced to a certificate that expired overnight. Renewed it and everything is deploying normally again. Sorry for the disruption, we will look at automating the renewal so this does not happen again.

Just merged the fix for the pagination bug that was reported last week. Thanks to everyone who helped narrow down the reproduction steps; that made this a fairly quick fix once we actually found it.

Taking the afternoon to write up documentation for the new account service, since three different people have asked me the same question about it this week alone.

Investigating a slow query that showed up in this morning's report. Nothing urgent yet, just want to understand it before it becomes a bigger problem.

Rolled out the search ranking change to a small percentage of traffic first. Numbers look promising so far; will expand gradually over the next few days if nothing unexpected turns up.

Thanks everyone for jumping on the call earlier, that was resolved faster than I expected given how confusing the initial symptoms were.
