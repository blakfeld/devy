# shadowenv init fixtures

The output of `shadowenv init bash`, `shadowenv init zsh` and `shadowenv init fish`
from shadowenv 3.4.0 (<https://github.com/Shopify/shadowenv>, tag `3.4.0`), captured
verbatim except that the absolute path of the shadowenv binary is replaced by
`@SHADOWENV@`. The shell hook tests in `tests/cli.rs` use them, with that placeholder
replaced by a fake `shadowenv`, always, and also with the installed shadowenv's init when
there is one, so devy's guard is tested against shadowenv's real init and hookbook even
on CI: `bash.sh` and `zsh.sh` in `shell_hook_guards_shadowenvs_own_init` (and `bash.sh`
in `shell_hook_guards_an_interactive_bash`), `fish.fish` in
`shell_hook_guards_shadowenvs_own_fish_init` (skipped when fish is not installed).

License: both pieces are MIT licensed by Shopify.

- The `__shadowenv_hook` functions come from shadowenv's `sh/shadowenv.*.in`
  (MIT License, Copyright (c) 2019 Shopify).
- The bash and zsh files embed Hookbook (<https://github.com/Shopify/hookbook>), whose
  copyright and MIT permission notice are kept in place in those files
  (Copyright 2019 Shopify Inc.).

To refresh them, run `shadowenv init <shell>` with the new version and replace the
binary's path with `@SHADOWENV@` again.

## shadowenv license

```text
The MIT License (MIT)

Copyright (c) 2019 Shopify

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```
