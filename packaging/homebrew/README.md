# Homebrew

`packaging/homebrew/koda.rb` is the formula. It installs the prebuilt binary
from the GitHub release, so `brew install` is a download rather than a Rust
toolchain and a two-minute compile.

## Making `brew install koda` work

Homebrew resolves a bare name against **homebrew-core** and against any tap the
user has added. There are two routes; the first is live.

### 1. The tap — live

Published at **[simpletoolsindia/homebrew-koda](https://github.com/simpletoolsindia/homebrew-koda)**,
serving `Formula/koda.rb` from this file:

```sh
brew tap simpletoolsindia/koda   # once, ever
brew install koda                # and from here it is the bare name
```

On Homebrew 6+, a third-party tap must be trusted once first, or tapping fails
with `Refusing to load formula ... from untrusted tap`:

```sh
brew trust simpletoolsindia/koda
```

### 2. homebrew-core (`brew install koda` with no tap at all)

This is the one that needs nothing from the user, and it has a bar. From
Homebrew's [Package Acceptance Policy](https://docs.brew.sh/Package-Acceptance-Policy),
a new package must show public interest beyond its author:

* 30 forks, 30 watchers **or** 75 stars; **or**
* 90 forks, 90 watchers **or** 225 stars **for a self-submission by the
  repository owner** — which is what a koda submission would be;
* and a repository at least 30 days old.

koda is at 0/0/0, so a pull request would be closed on sight. The formula name
`koda` is unclaimed in homebrew-core, so it is still there when the project
qualifies.

When it does:

```sh
brew tap-new --no-git homebrew/core           # if you do not have it locally
brew audit --strict --online --new koda       # must pass clean
brew bump-formula-pr --url <release tarball> --sha256 <sum> koda
```

A homebrew-core formula also has to **build from source** rather than install a
binary, so the formula gets a `depends_on "rust" => :build` and a
`system "cargo", "install", *std_cargo_args` install block at that point.

## Keeping it current

Every release changes a version and four checksums:

```sh
packaging/update.py v0.2.0            # rewrite the formula from that release
packaging/update.py v0.2.0 --check    # CI: fail if it was forgotten
```

## Testing a change

Homebrew will not audit a formula outside a tap, so use a throwaway one:

```sh
brew tap-new simpletoolsindia/koda --no-git
cp packaging/homebrew/koda.rb "$(brew --repository simpletoolsindia/koda)/Formula/koda.rb"
brew style simpletoolsindia/koda
brew audit --strict --online simpletoolsindia/koda/koda
brew install simpletoolsindia/koda/koda && koda --version
brew uninstall koda && brew untap simpletoolsindia/koda
```
