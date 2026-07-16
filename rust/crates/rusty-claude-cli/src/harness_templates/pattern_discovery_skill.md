---
name: pattern-discovery
description: Search first, reuse always, create only when necessary — find existing implementations before writing new code.
---
# Pattern discovery

Before writing any new function, module, or file, search the codebase for
something that already does what you need (or close to it).

## Search First

- Grep for the behavior you need, not just the name you'd give it: try
  several likely keywords, synonyms, and the domain terms the codebase
  already uses.
- Glob for files by naming convention (`*_test.rs`, `**/handlers/*.ts`, etc.)
  to see how similar problems were solved elsewhere in this repo.
- Check for existing helpers, utilities, or abstractions one directory up
  and one directory down from where you're working before assuming there
  are none.

## Reuse Always

- If something close to what you need already exists, extend or
  parameterize it rather than duplicating its logic.
- Match the existing code's conventions (naming, error handling, module
  layout) when you do extend it.

## Create Only When Necessary

- Only write new code once you've confirmed nothing suitable exists.
- When you do create something new, make it easy for the next search to
  find: clear naming, a doc comment describing what it does and why it's
  separate from anything similar.
