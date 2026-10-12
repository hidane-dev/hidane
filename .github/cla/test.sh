#!/usr/bin/env bash
# The cases quote shell syntax on purpose: commit messages are data, not commands.
# shellcheck disable=SC2016
# Scenario tests for .github/cla/check.sh. Each case runs the check in a dry run against a fake
# `gh` that serves the case's GitHub data from files, then checks the status it would set, what it
# would write, and what it would say. Run from the repository root: bash .github/cla/test.sh
set -euo pipefail

root=$(pwd)
work=$(mktemp -d)
# KEEP=1 keeps the cases' files, to look at what the check would have written.
if [[ -n "${KEEP:-}" ]]; then echo "keeping $work"; else trap 'rm -rf "$work"' EXIT; fi
mkdir -p "$work/bin"

# The fake gh: `gh api <endpoint> [--jq <expr>] ...`. Reads come from $CASE_DIR; anything else
# fails like a 404. Every call is logged to $CASE_DIR/calls.log.
cat > "$work/bin/gh" <<'FAKE'
#!/usr/bin/env bash
set -uo pipefail
printf '%s\n' "$*" >> "$CASE_DIR/calls.log"
shift # api
endpoint="" expr=""
while (($#)); do
  case "$1" in
    --jq) expr=$2; shift 2 ;;
    -X|-f|-F|--input) shift 2 ;;
    --paginate) shift ;;
    *) [[ -z "$endpoint" ]] && endpoint=$1; shift ;;
  esac
done
case "$endpoint" in
  graphql) file=graphql.json ;;
  repos/*/pulls/*/commits) file=commits.json ;;
  repos/*/pulls/*) file=pr.json ;;
  repos/*/issues/*/comments) file=comments.json ;;
  repos/*/git/ref/heads/cla-signatures) [[ -e "$CASE_DIR/records" ]] && exit 0 || exit 1 ;;
  repos/*/contents/*)
    name=${endpoint#repos/*/contents/}; name=${name%%\?*}
    [[ -f "$CASE_DIR/records/$name" ]] || exit 1
    jq -n --arg c "$(base64 < "$CASE_DIR/records/$name" | tr -d '\n')" '{sha: "blob", content: $c}'
    exit 0 ;;
  users/*) file="users/$(sed 's/%5B/[/g; s/%5D/]/g' <<<"${endpoint#users/}").json" ;;
  *) exit 1 ;;
esac
[[ -f "$CASE_DIR/$file" ]] || { [[ $file == comments.json ]] && echo '[]' > "$CASE_DIR/$file"; } || exit 1
if [[ -n "$expr" ]]; then jq -r "$expr" "$CASE_DIR/$file"; else cat "$CASE_DIR/$file"; fi
FAKE
chmod +x "$work/bin/gh"

failures=0
# A case: a directory with pr.json, commits.json (the REST list), graphql.json, users/<login>.json
# and records/<file>.json for the cla-signatures branch.
new_case() {
  CASE_DIR="$work/$1"
  mkdir -p "$CASE_DIR/users"
  export CASE_DIR
  user alice 101 User ""
  user bob 102 User ""
  user nomunomu0504 9 User ""
  user carol 103 User "Acme Corp"
  user "dependabot[bot]" 49699333 Bot ""
  user "github-actions[bot]" 41898282 Bot ""
  pr alice 101 User
  commits '[]'
}
user() { jq -n --arg l "$1" --argjson id "$2" --arg t "$3" --arg c "$4" \
  '{login: $l, id: $id, type: $t, company: (if $c == "" then null else $c end)}' > "$CASE_DIR/users/$1.json"; }
pr() { jq -n --arg l "$1" --argjson id "$2" --arg t "$3" \
  '{state: "open", head: {sha: "headsha"}, commits: null, user: {login: $l, id: $id, type: $t}}' > "$CASE_DIR/pr.json"; }
# commits '[{sha, login|null, id|null, type, name, email, message, coauthors: [{name, email, login, id}]}]'
commits() {
  jq '[.[] | {sha, commit: {message, author: {name, email}},
             author: (if .login then {login, id, type} else null end)}]' <<<"$1" > "$CASE_DIR/commits.json"
  jq --argjson n "$(jq length <<<"$1")" '.commits = $n' "$CASE_DIR/pr.json" > "$CASE_DIR/pr.json.new"
  mv "$CASE_DIR/pr.json.new" "$CASE_DIR/pr.json"
  jq '{data: {repository: {pullRequest: {commits: {nodes: [.[] | {commit: {oid: .sha, authors: {nodes:
        ([{name, email, user: (if .login then {login, databaseId: .id} else null end)}]
         + [(.coauthors // [])[] | {name, email, user: (if .login then {login, databaseId: .id} else null end)}])}}}]}}}}}' \
    <<<"$1" > "$CASE_DIR/graphql.json"
}
signed() { # signed <login> <id> [employer_check]
  mkdir -p "$CASE_DIR/records"
  local file="$CASE_DIR/records/individual.json"
  [[ -f "$file" ]] || echo '{"signatures": []}' > "$file"
  jq --arg l "$1" --argjson id "$2" --argjson e "${3:-false}" \
    '.signatures += [{login: $l, id: $id, agreement: "individual", version: "1.0", employer_check: $e}]' "$file" > "$file.new"
  mv "$file.new" "$file"
}
records() { mkdir -p "$CASE_DIR/records"; printf '%s\n' "$2" > "$CASE_DIR/records/$1"; }

# Runs the check; $1 and $2 are an optional comment body and commenter (login:id).
run() {
  local body=${1:-} who=${2:-:0}
  (cd "$root" && env PATH="$work/bin:$PATH" DRY_RUN=1 REPO=hidane-dev/hidane PR=7 \
    COMMENT_BODY="$body" COMMENTER_LOGIN="${who%%:*}" COMMENTER_ID="${who##*:}" \
    COMMENT_URL="https://example.invalid/c" bash .github/cla/check.sh) \
    > "$CASE_DIR/out" 2> "$CASE_DIR/err" || { echo "check.sh failed:"; cat "$CASE_DIR/err"; return 1; }
}
expect() { # expect <description> <state> [<text in stdout+stderr>...]
  local name=$1 state=$2 ok=1 text
  shift 2
  grep -q "^cla: $state " "$CASE_DIR/out" || ok=0
  for text in "$@"; do
    if [[ $text == '!'* ]]; then grep -qF -- "${text#!}" "$CASE_DIR/out" "$CASE_DIR/err" && ok=0
    else grep -qF -- "$text" "$CASE_DIR/out" "$CASE_DIR/err" || ok=0; fi
  done
  if ((ok)); then echo "ok   $name"; else
    echo "FAIL $name"; sed 's/^/     /' "$CASE_DIR/out"; failures=$((failures + 1)); fi
}
c() { jq -n --arg sha "$1" --arg login "$2" --arg name "$3" --arg email "$4" --arg message "$5" --argjson co "${6:-[]}" \
  '{sha: $sha, login: (if $login == "" then null else $login end),
    id: ({"alice": 101, "bob": 102, "carol": 103, "nomunomu0504": 9, "dependabot[bot]": 49699333}[$login]),
    type: (if ($login | endswith("[bot]")) then "Bot" else "User" end),
    name: $name, email: $email, message: $message, coauthors: $co}'; }
statement='I have read the hidane Individual CLA 1.0, I have the right to agree to it for my contributions, and I agree to it.'

new_case a-single-signed
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com 'fix: a')]"
signed alice 101
run; expect "A: one author who agreed passes" success "Every author has agreed"

new_case b-single-unsigned
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com 'fix: a')]"
run; expect "B: one author who has not agreed fails" failure "Waiting for 1 author(s)" "waiting for @alice"

new_case b2-statement
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com 'fix: a')]"
run "$statement" alice:101
expect "B: the statement records the agreement and passes" success "contents/individual.json" "alice agrees to the Individual CLA 1.0 in #7"

new_case c-coauthor-signed
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: Bob <bob@gmail.com>' \
  '[{"name": "Bob", "email": "bob@gmail.com", "login": "bob", "id": 102}]')]"
signed alice 101; signed bob 102
run; expect "C: author and co-author who both agreed pass" success "Every author has agreed"

new_case d-coauthor-unsigned
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: Bob <bob@gmail.com>' \
  '[{"name": "Bob", "email": "bob@gmail.com", "login": "bob", "id": 102}]')]"
signed alice 101
run; expect "D: a co-author who has not agreed fails" failure "waiting for @bob" "!waiting for @alice"

new_case d2-trailer-github-missed
# The trailer is not in the last paragraph, so GitHub does not list it; the check still reads it.
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-Authored-By:   Bob <102+bob@users.noreply.github.com>\n\nmore text')]"
signed alice 101
run; expect "D: a trailer GitHub does not list is still read" failure "waiting for @bob"

new_case e-bots
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'chore: a\n\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>\nCo-authored-by: Claude <noreply@anthropic.com>'), $(c aaaaaaa2 'dependabot[bot]' dependabot 49699333+dependabot[bot]@users.noreply.github.com 'chore(deps): bump')]"
signed alice 101
run; expect "E: GitHub App and listed non-human co-authors are not asked" success "Every author has agreed" "!manual review"

new_case e2-bot-pr
pr "dependabot[bot]" 49699333 Bot
commits "[$(c aaaaaaa1 'dependabot[bot]' dependabot 49699333+dependabot[bot]@users.noreply.github.com 'chore(deps): bump')]"
run; expect "E: a pull request from a GitHub App needs no agreement" success "No agreement needed"

new_case e3-name-is-not-enough
# An account named like a bot but of type User is a person.
user "notabot[bot]" 555 User ""
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: X <555+notabot[bot]@users.noreply.github.com>')]"
signed alice 101
run; expect "E: a name ending in [bot] alone does not exempt anyone" failure "waiting for @notabot[bot]"

new_case f-unidentified
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: John Doe <john@example.com>' \
  '[{"name": "John Doe", "email": "john@example.com", "login": "", "id": null}]')]"
signed alice 101
run; expect "F: a co-author linked to no account needs manual review" failure "1 author(s) need manual review" "CLA verification requires manual review" '`John Doe` `<john@example.com>`'

new_case f2-no-email
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: Somebody')]"
signed alice 101
run; expect "F: a trailer without an email address needs manual review" failure "need manual review"

new_case f3-noreply-id-mismatch
# A private address whose ID is not the account's is not trusted.
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: Bob <999+bob@users.noreply.github.com>')]"
signed alice 101
run; expect "F: a private address with the wrong ID is not matched" failure "need manual review" "!waiting for @bob"

new_case f4-offline-by-maintainer
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: John Doe <John@Example.com>')]"
signed alice 101
run "/cla signed-offline john@example.com: signed by email, kept on file" nomunomu0504:9
expect "F: a maintainer records an agreement made outside GitHub" success "contents/reviews.json" "signed-offline"

new_case f5-offline-not-maintainer
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: John Doe <john@example.com>')]"
signed alice 101
run "/cla signed-offline john@example.com: trust me" alice:101
expect "F: only a maintainer records a review" failure "only a maintainer records reviews" "!contents/reviews.json"

new_case g-employer-open
pr carol 103 User
commits "[$(c aaaaaaa1 carol Carol carol@acme.example 'feat: c')]"
signed carol 103
run; expect "G: a signed author with signs of an employer needs review" failure "Employer authorization needs review for 1" "**needs review**: @carol" 'profile: `Acme Corp`' 'email: `acme.example`'

new_case g2-employer-personal
pr carol 103 User
commits "[$(c aaaaaaa1 carol Carol carol@acme.example 'feat: c')]"
signed carol 103
run "/cla employer @carol personal: written on a weekend, own equipment, confirmed in this thread" nomunomu0504:9
expect "G: a maintainer records outcome A (personal)" success "employer authorization reviewed" '"outcome":"personal","scope":"pull_request"'

new_case g3-employer-standing
pr carol 103 User
commits "[$(c aaaaaaa1 carol Carol carol@acme.example 'feat: c')]"
signed carol 103
records reviews.json '{"reviews": [{"kind": "employer", "login": "carol", "id": 103, "outcome": "authorized", "scope": "standing", "pull_request": 3, "reviewed_by": "nomunomu0504"}]}'
run; expect "G: a standing authorization from an earlier pull request applies" success "employer authorization reviewed"

new_case g4-employer-other-pr
pr carol 103 User
commits "[$(c aaaaaaa1 carol Carol carol@acme.example 'feat: c')]"
signed carol 103
records reviews.json '{"reviews": [{"kind": "employer", "login": "carol", "id": 103, "outcome": "personal", "scope": "pull_request", "pull_request": 3, "reviewed_by": "nomunomu0504"}]}'
run; expect "G: a review for another pull request does not apply" failure "Employer authorization needs review"

new_case g5-corporate
pr carol 103 User
commits "[$(c aaaaaaa1 carol Carol carol@acme.example 'feat: c')]"
records corporate.json '{"agreements": [{"company": "Acme Corp", "designated": [{"login": "carol", "id": 103}]}]}'
run; expect "G: a Designated Contributor under a Corporate CLA passes (outcome C)" success "Corporate CLA"

new_case g6-sign-persists
# The profile no longer names a company, but it did when they agreed.
pr bob 102 User
commits "[$(c aaaaaaa1 bob Bob bob@users.noreply.github.com 'feat: b')]"
signed bob 102 true
run; expect "G: signs seen when agreeing still need review" failure "Employer authorization needs review" "noted when agreeing"

new_case g7-not-maintainer-command
pr carol 103 User
commits "[$(c aaaaaaa1 carol Carol carol@acme.example 'feat: c')]"
signed carol 103
run "/cla employer @carol personal: it is mine" carol:103
expect "G: an author cannot clear their own employer review" failure "only a maintainer records reviews"

new_case h2-assistant-linked
# GitHub links the assistant's address to an account; it is still not a person.
user claude 81847 User ""
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>' \
  '[{"name": "Claude Opus 5.5", "email": "noreply@anthropic.com", "login": "claude", "id": 81847}]')]"
signed alice 101
run; expect "H: a listed non-human co-author linked to an account is not asked" success "Every author has agreed" "!@claude"

new_case h-maintainer-only
pr nomunomu0504 9 User
commits "[$(c aaaaaaa1 nomunomu0504 'Hiroki Nomura' 9+nomunomu0504@users.noreply.github.com $'fix: x\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>')]"
run; expect "H: the maintainer with an AI co-author needs no agreement" success "No agreement needed"

new_case i-hostile-input
# Commit messages and names are data: nothing in them runs, and the comment quotes them inertly.
hostile=$(printf 'fix: a\n\nCo-authored-by: $(touch %s/pwned) `x` @everyone <evil$(id)@example.com>\nCo-authored-by: Y <1+../../repos/x@users.noreply.github.com>\nCo-authored-by: Z <2+a;b@users.noreply.github.com>' "$work")
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com "$hostile")]"
signed alice 101
run; expect "I: hostile trailers run nothing and are quoted" failure "3 author(s) need manual review" '$(touch' '<evil$(id)@example.com>' "!users/..%2F" "!users/../"
if [[ -e "$work/pwned" ]] || grep -q 'users/\.\./\|users/a;b' "$CASE_DIR/calls.log"; then
  echo "FAIL I: a hostile trailer had an effect"; failures=$((failures + 1)); fi

new_case k-too-many-commits
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com 'fix: a')]"
signed alice 101
jq '.commits = 300' "$CASE_DIR/pr.json" > "$CASE_DIR/pr.json.new"; mv "$CASE_DIR/pr.json.new" "$CASE_DIR/pr.json"
run; expect "K: a pull request with more commits than GitHub lists fails" failure "too many commits"

new_case j-statement-by-coauthor
commits "[$(c aaaaaaa1 alice Alice alice@users.noreply.github.com $'fix: a\n\nCo-authored-by: Bob <bob@gmail.com>' \
  '[{"name": "Bob", "email": "bob@gmail.com", "login": "bob", "id": 102}]')]"
signed alice 101
run "$statement" bob:102
expect "J: a co-author agrees by posting the statement" success "bob agrees to the Individual CLA 1.0 in #7"

if ((failures > 0)); then
  echo "$failures case(s) failed"
  exit 1
fi
echo "all cases passed"
