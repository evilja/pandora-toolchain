# README template guide

This file is the **README template** (`base.md`). When a repo is bootstrapped
with `/init` or `/attach` (or the git console's Init / Attach), this template is
rendered into the repo's `README.md`.

Set a per-server template in Discord with `/readmebase` (it is stored at
`DB/config/<server_id>/base.md`). When a server has no template of its own, this
guide — `DB/config/global/base.md` — is shown instead.

## Variables

Write a variable as `%name%`. Every occurrence is replaced when the README is
rendered:

| Variable                  | Meaning                                              |
| ------------------------- | --------------------------------------------------- |
| `%name%`                  | Anime title (from MyAnimeList)                       |
| `%slug%`                  | URL-safe slug of the title                           |
| `%kind%`                  | `Movie` or `MultiEpisode`                            |
| `%mal_id%`                | MyAnimeList id                                       |
| `%episode_count%`         | Total episode count                                  |
| `%year%`                  | Release year (may be empty)                          |
| `%repo_url%`              | Full Forgejo repo URL                                |
| `%episode_count_at_git%`  | Episode folders already present in the repo          |
| `%season%`                | Season / sequel number (1-based)                     |
| `%tl%`                    | Translator credit (`---` if unset)                  |
| `%tlc%`                   | Translation-check credit (`---` if unset)           |
| `%ts%`                    | Typesetter credit (`---` if unset)                  |
| `%qc%`                    | Quality-check credit (`---` if unset)               |

## Formatting

- The file is plain **Markdown** — anything Forgejo renders works (headings,
  lists, tables, links, images, badges).
- An unknown `%foo%` is left untouched, so a literal percent sign is fine as long
  as it is not a real variable name.
- Credits default to `---` when no value was provided, so a credits line always
  renders cleanly.

## Example

```markdown
# %name% (S%season%)

[%name% on MyAnimeList](https://myanimelist.net/anime/%mal_id%) · %kind% · %year%

A %episode_count%-episode release. Source & subtitles live in this repo:
%repo_url%

## Credits

| Role | Member  |
| ---- | ------- |
| TL   | %tl%    |
| TLC  | %tlc%   |
| TS   | %ts%    |
| QC   | %qc%    |
```
