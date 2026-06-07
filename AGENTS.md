# The TIR project guidelines

## Coding guidelines

1. Think before coding. Do not assume anything. Verify, don't hide confusion.
   Consider tradeoffs. Consult with the user when the task is unclear. If
   multiple interpretations exist, present them clearly - don't pick silently.
2. Strive for simplicity. Produce the minimum amount of code required to solve
   the problem. No features beyond what's asked. No abstractions for single-use
   code. No flexibility or configurability that was not requested. No error
   handling for impossible situations. If the solution of 200 lines of code can
   be done in 50 - rewrite it. This does not give you permission to cheat.
   Resolve core issue end-to-end. If a user asks you to fix a test, you are not
   allowed to simply disable an assertion or entire test - the fix must be genuine. 
3. Touch only those pieces of existing code that are relevant to the fix. Don't
   improve adjacent code unless explicitly asked. If you need to refactor existing
   interfaces, ask user first. Match existing style always. If you see existing
   unrelated dead code - highlight that, but don't delete silently.
4. Pair your changes with reasonable testing. Treat tests as a code expression
   of success criteria for user goals. Loop until the goal is reached and verified
   by testing. If the task is to refactor something, make sure existing tests work
   both before and after your changes. For complex tasks follow these instructions
   for each stage.
5. Keep code tidy. Remove imports/variables/functions that YOUR changes made
   unused. Don't remove pre-existing dead code unless asked. Every changed line
   should trace directly to the user's request. After all changes are done and
   all tests are passing, run formatting routines and linters. Fix all warnings.
   Do not put everything in a giant file. Split large functions to be no more
   than 400 lines. Respect responsibility ownership between modules.
6. Make your answers short and simple and on task. Do not apologize, do not try
   to be polite, do not explain yourself unless explicitly asked. When writing
   code, avoid obvious commentary - do not explain what the code does. If needed,
   you can add a comment that explains non-obvious design decisions. Such comments
   should answer "Why?" not "What?". Conserve your token budget and avoid any
   kind of duplication. Code must explain itself without additions.
7. Keep prose terse - this is the rule that matters, enforce it on every word you
   show the user. No preamble, no filler (actually, however, basically), no
   restating the question, no self-narration. Prefer shorter synonyms (big, not
   extensive). This applies to all output: chat replies, summaries, PR text,
   commit bodies. Always use normal English in code comments, warnings, security
   messages, and when addressing the user directly.
8. During internal reasoning, if the harness lets you steer it, skip articles,
   prepositions, conjunctions and other noise - caveman style to save tokens.
   EXAMPLE: not "I have enough information now to implement new instruction. Let
   me synthesize what I learned from exploring the repository.", instead
   "Implement instruction platform RISC-V".
9. Use conventional commits v1.0.0 spec for commit titles and descriptions.

## Working with code

- `cargo build`: build Rust code
- `cargo test`: run Rust tests
- `cargo fmt`: automatically format Rust code
- `cargo clippy`: Rust linter
