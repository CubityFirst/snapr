# Contributing to snapr

Thanks for helping. Bug reports, ideas and pull requests are all welcome on
GitHub.

## Commit messages

snapr uses [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/).
Each release's notes are built from the commits that went into it
(`scripts/release-notes.sh`), so a commit's subject is its changelog line.

```
<type>[(scope)][!]: <description>

[body]

[footers]
```

- **type**: what kind of change it is. The release notes group commits by
  type:

  | Type | For | In the release notes under |
  |---|---|---|
  | `feat` | something new people can do | New |
  | `fix` | something that was wrong and now isn't | Fixes |
  | `perf` | the same, faster or lighter | Faster |
  | `refactor` | code changes that don't change what snapr does | Other changes |
  | `docs` | README, this file, comments | Other changes |
  | `style` | formatting only | Other changes |
  | `test` | tests | Other changes |
  | `build` | Cargo, dependencies, packaging | Other changes |
  | `ci` | GitHub workflows | Other changes |
  | `chore` | anything else, and version bumps | Other changes |
  | `revert` | undoing an earlier commit | Other changes |

- **scope** (optional): the part of snapr it's about, lowercase, e.g.
  `windows`, `macos`, `linux`, `overlay`, `record`, `annotate`, `upload`,
  `tray`, `settings`, `update`. Platform scopes show as **Windows**,
  **macOS** and **Linux** in the release notes.
- **`!`**: marks a breaking change, e.g. settings or file names that no
  longer carry over. Say what breaks in the body or in a
  `BREAKING CHANGE:` footer.
- **description**: what changes for someone using snapr, in the present
  tense, starting lowercase with no full stop. The release notes capitalise
  it.
- **body** (optional): why, and anything a user or a reviewer should know.
  Wrapped at 72 columns. It stays in the history; the release notes show
  only the description.
- **footers** (optional): credits and references, as Git trailers.
  Credit whoever reported or suggested the change, with a link:
  `Reported-by: Name (https://github.com/name)`.

For example:

```
fix(windows): recordings no longer begin with the capture overlay fading out

The overlay's close animation is off, and a recording waits for the screen
to be drawn without the overlay before taking its first frame.

Reported-by: Neppkun (https://github.com/Neppkun)
```

```
feat(macos): the capture overlay fades in where it is
```

Commits that aren't Conventional Commits fail the *Commits* check on pull
requests and pushes to `main`. To have Git check each message as you
commit, turn on the repository's hook once:

```
git config core.hooksPath .githooks
```

## Releases

Maintainers cut releases: bump `version` in `Cargo.toml` and `Cargo.lock`
in a `chore(release): snapr X.Y.Z` commit, which the release notes leave
out, then tag and push it:

```
git tag -a v0.2.0 -m "snapr 0.2.0"
git push origin main v0.2.0
```

The *Release* workflow (`.github/workflows/release.yml`) builds Windows
x86-64, macOS (Apple silicon only) and Linux x86-64, and publishes them
as a GitHub release named after the tag, with a `SHA256SUMS` file and
notes made from the commits since the last release
(`scripts/release-notes.sh`). The macOS build is packed as `snapr.app`
(`scripts/macos-bundle.sh`), published as a `.dmg` to install from and a
zip the updater swaps the whole app for. It refuses a tag that doesn't
match `Cargo.toml`'s version (such a build would keep offering itself as an
update). A tag with a hyphen, e.g. `v0.2.0-rc.1`, is published as a
pre-release.
