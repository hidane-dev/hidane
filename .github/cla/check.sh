#!/usr/bin/env bash
# The cla check (.github/workflows/cla.yml, docs/cla/README.md).
#
# Every person who wrote a pull request's commits must be covered by a CLA: each commit's author,
# each co-author named in a `Co-authored-by:` trailer, and the pull request's author. Covered means
# agreed to the Individual CLA (recorded on the cla-signatures branch) or a Designated Contributor
# under a Corporate CLA. Exempt: the maintainers (.github/cla/exempt), GitHub App accounts (type
# Bot), and co-authors that are not people, listed by email in .github/cla/non-human-co-authors.
#
# A person is identified only as GitHub identifies them: a commit's author by the account GitHub
# links its email to, a co-author by the account GitHub links the trailer's email to, or by a
# GitHub private address (<id>+<login>@users.noreply.github.com) whose account matches. Anyone
# else blocks the pull request until fixed, or until a maintainer records that they agreed outside
# GitHub (`/cla signed-offline <email>: <note>`).
#
# Someone whose GitHub profile names a company, who writes with an email address outside
# .github/cla/personal-email-domains, or who showed either sign when agreeing, may be writing for
# an employer with rights in the work. The check fails until that is resolved: they are a
# Designated Contributor under a Corporate CLA, or a maintainer records the outcome of asking
# them, `/cla employer @login personal: <note>` (their own work, for this pull request),
# `/cla employer @login authorized: <note>` (the employer allowed this pull request) or
# `/cla employer @login authorized-standing: <note>` (the employer allowed their contributions).
# Maintainers' records go to reviews.json on the cla-signatures branch.
#
# Then the `cla` status is set on the pull request's head, and one comment on it is kept up to
# date. Everything read from the pull request (commit messages, names, email addresses, profiles,
# comments) is data: it goes through jq arguments and quoted variables, never through eval, and
# is cleaned before it is quoted in the comment.
#
# Environment: GH_TOKEN, REPO (owner/name) and PR (number); for a comment, COMMENT_BODY,
# COMMENTER_LOGIN, COMMENTER_ID and COMMENT_URL. DRY_RUN=1 prints what would be written instead.
set -euo pipefail

branch=cla-signatures
document=docs/cla/individual.md
version=1.0
statement='I have read the hidane Individual CLA 1.0, I have the right to agree to it for my contributions, and I agree to it.'
label='cla: employer check'
marker='<!-- hidane-cla -->'
readme="https://github.com/$REPO/blob/main/docs/cla/README.md"
exempt_file=${EXEMPT_FILE:-.github/cla/exempt}
personal_file=.github/cla/personal-email-domains
non_human_file=.github/cla/non-human-co-authors
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
records_readme='# hidane CLA records

Agreements to the hidane Contributor License Agreements and the maintainers reviews, recorded by
the cla check (.github/workflows/cla.yml on main). See docs/cla/README.md on main.'

# Writes go through here, so that a dry run writes nothing.
write() {
  if [[ "${DRY_RUN:-}" == 1 ]]; then
    echo "dry run: gh api $*" >&2
    if [[ " $* " == *" --input - "* ]]; then cat >&2; fi
  else
    gh api "$@"
  fi
}

# A JSON file of the cla-signatures branch as {sha, data}: its blob and content, or an empty sha
# and $2 when the branch or the file is missing.
read_record() {
  local response
  if response=$(gh api "repos/$REPO/contents/$1?ref=$branch" 2>/dev/null); then
    jq -c '{sha, data: (.content | gsub("\n"; "") | @base64d | fromjson)}' <<<"$response"
  else
    jq -cn --argjson empty "$2" '{sha: "", data: $empty}'
  fi
}

# Appends the record $3 to the array $2 of the file $1 on the cla-signatures branch, creating the
# branch if needed, with the commit message $4, and prints the file's new content.
append_record() {
  local file=$1 key=$2 record=$3 message=$4 stored data sha updated tree commit attempt
  if ! gh api "repos/$REPO/git/ref/heads/$branch" >/dev/null 2>&1; then
    # The branch holds the records only: it starts from a commit of its own.
    tree=$(jq -n --arg readme "$records_readme" \
      '{tree: [{path: "README.md", mode: "100644", type: "blob", content: $readme}]}' \
      | write "repos/$REPO/git/trees" --input - --jq .sha)
    commit=$(jq -n --arg tree "$tree" '{message: "Start the record of CLA agreements", tree: $tree, parents: []}' \
      | write "repos/$REPO/git/commits" --input - --jq .sha)
    write "repos/$REPO/git/refs" -f ref="refs/heads/$branch" -f sha="$commit" >/dev/null
  fi
  [[ "${DRY_RUN:-}" == 1 ]] && echo "dry run: record in $file: $(jq -c . <<<"$record")" >&2
  for attempt in 1 2 3; do
    stored=$(read_record "$file" "$(jq -cn --arg key "$key" '{($key): []}')")
    data=$(jq -c .data <<<"$stored")
    sha=$(jq -r .sha <<<"$stored")
    updated=$(jq -c --arg key "$key" --argjson record "$record" '.[$key] += [$record]' <<<"$data")
    if jq -n --arg message "$message" \
      --arg content "$(jq . <<<"$updated" | base64 | tr -d '\n')" --arg branch "$branch" --arg sha "$sha" \
      '{message: $message, content: $content, branch: $branch} + (if $sha == "" then {} else {sha: $sha} end)' \
      | write -X PUT "repos/$REPO/contents/$file" --input - >/dev/null; then
      echo "$updated"
      return
    fi
    echo "attempt $attempt: $file changed meanwhile, reading it again" >&2
  done
  exit 1
}

list() { { grep -v -e '^#' -e '^[[:space:]]*$' "$1" || true; } | jq -R 'gsub("^\\s+|\\s+$"; "") | ascii_downcase' | jq -cs .; }
exempt=$(list "$exempt_file")
personal=$(list "$personal_file")
non_human=$(list "$non_human_file")
lower() { tr '[:upper:]' '[:lower:]' <<<"$1"; }

pr=$(gh api "repos/$REPO/pulls/$PR")
if [[ $(jq -r .state <<<"$pr") != open ]]; then
  echo "#$PR is not open"
  exit 0
fi
head=$(jq -r .head.sha <<<"$pr")

# The commits: each one's author as GitHub links it to an account, and its message. GitHub lists at
# most 250 commits of a pull request; one with more cannot be checked whole, so it fails.
commits=$(gh api --paginate "repos/$REPO/pulls/$PR/commits" --jq '.[] | {oid: .sha, message: .commit.message,
  author: {name: .commit.author.name, email: (.commit.author.email // ""),
           login: .author.login, id: .author.id, type: .author.type}}' | jq -cs .)
if [[ $(jq length <<<"$commits") != $(jq -r '.commits // 0' <<<"$pr") ]]; then
  write -X POST "repos/$REPO/statuses/$head" -f state=failure -f context=cla \
    -f description="Too many commits to check: split or squash the pull request" -f target_url="$readme" >/dev/null
  echo "cla: failure (too many commits to check)"
  exit 0
fi
# The co-authors GitHub reads from the trailers, with the account each email is linked to.
# shellcheck disable=SC2016 # GraphQL variables, not shell ones
coauthors=$(gh api graphql --paginate -f owner="${REPO%%/*}" -f name="${REPO#*/}" -F pr="$PR" -f query='
  query($owner: String!, $name: String!, $pr: Int!, $endCursor: String) {
    repository(owner: $owner, name: $name) {
      pullRequest(number: $pr) {
        commits(first: 100, after: $endCursor) {
          pageInfo { hasNextPage endCursor }
          nodes { commit { oid authors(first: 100) { nodes { name email user { login databaseId } } } } }
        }
      }
    }
  }' --jq '.data.repository.pullRequest.commits.nodes[].commit
    | {oid, nodes: [.authors.nodes[1:][] | {name, email: (.email // ""), login: .user.login, id: .user.databaseId}]}' | jq -cs .)

# Everyone named in the commits. The trailers are also read from the messages, so that one GitHub
# does not recognise is not missed; a trailer without an email address cannot name anyone.
people=$(jq -c --argjson coauthors "$coauthors" '[.[] | .oid as $oid
  | ([$coauthors[] | select(.oid == $oid) | .nodes[] | . + {type: "User"}]) as $known
  | [.author + {role: "author"}]
    + [$known[] | . + {role: "co-author"}]
    + [.message | split("\n")[] | select(test("^\\s*co-authored-by\\s*:"; "i"))
       | (capture("^\\s*co-authored-by\\s*:\\s*(?<name>.*?)\\s*<(?<email>[^<>\\s]+)>\\s*$"; "i")
          // {name: sub("^\\s*co-authored-by\\s*:\\s*"; ""; "i"), email: ""})
       | {name, email, login: null, id: null, type: null, role: "co-author"}
       | select(.email == "" or (.email | ascii_downcase) as $e | $known | map(.email | ascii_downcase) | index($e) | not)]
  | .[] | . + {sha: $oid[0:7], email: (.email | ascii_downcase)}]
  | unique_by([.sha, .role, .email, .name])' <<<"$commits")

# Co-authors that are not people need no agreement, whether or not their email address is linked
# to a GitHub account.
people=$(jq -c --argjson non_human "$non_human" \
  'map(select(.role == "author" or (.email as $e | $non_human | index($e) | not)))' <<<"$people")

# A GitHub private address names its account; it is trusted only when GitHub returns an account
# for that login, with the same ID when the address carries one.
while IFS=$'\t' read -r email want login; do
  [[ "$login" =~ ^[A-Za-z0-9-]+(\[bot\])?$ ]] || continue
  if user=$(gh api "users/$(jq -rn --arg l "$login" '$l | @uri')" 2>/dev/null) \
    && { [[ -z "$want" ]] || [[ "$(jq -r .id <<<"$user")" == "$want" ]]; }; then
    people=$(jq -c --arg email "$email" --argjson user "$user" \
      'map(if .email == $email and .id == null then . + {login: $user.login, id: $user.id, type: $user.type} else . end)' <<<"$people")
  fi
done < <(jq -r '.[] | select(.id == null) | .email
  | (capture("^((?<id>[0-9]+)\\+)?(?<login>[^@+]+)@users\\.noreply\\.github\\.com$") // null) as $m
  | select($m != null) | [., ($m.id // ""), $m.login] | @tsv' <<<"$people" | sort -u)

# Who needs an agreement: people with an account, but not the maintainers nor GitHub Apps; and
# who cannot be identified: people without one.
authors=$(jq -c --argjson pr "$pr" --argjson exempt "$exempt" '
  [{login: $pr.user.login, id: $pr.user.id, type: $pr.user.type}] + [.[] | select(.id != null) | {login, id, type}]
  | unique_by(.id)
  | map(select(.type != "Bot" and ((.login | ascii_downcase) as $l | $exempt | index($l) | not)))' <<<"$people")
unidentified=$(jq -c '[.[] | select(.id == null) | {sha, role, name, email}]' <<<"$people")

stored=$(read_record individual.json '{"signatures": []}')
individual=$(jq -c .data <<<"$stored")
corporate=$(read_record corporate.json '{"agreements": []}' | jq -c .data)
reviews=$(read_record reviews.json '{"reviews": []}' | jq -c .data)

# Signs of an employer: the company on each author's profile, the domains of their commit email
# addresses that are not personal ones, and whether they showed either sign when they agreed.
signals='[]'
while IFS=$'\t' read -r login id; do
  company=$(gh api "users/$(jq -rn --arg l "$login" '$l | @uri')" --jq '.company // ""')
  signals=$(jq -c --arg login "$login" --argjson id "$id" --arg company "$company" \
    --argjson people "$people" --argjson personal "$personal" --argjson individual "$individual" '
    . + [{login: $login, id: $id,
          company: ($company | gsub("^\\s+|\\s+$"; "") | if . == "" then null else . end),
          domains: ([$people[] | select(.id == $id) | .email | split("@") | .[1] // ""] | unique
                    | map(select(. != "" and (. as $d | $personal | index($d) | not)))),
          when_agreeing: any($individual.signatures[]; .id == $id and .employer_check == true)}]' <<<"$signals")
done < <(jq -r '.[] | [.login, .id] | @tsv' <<<"$authors")

# Who is covered, whose employer's rights are resolved, and who is still unidentified.
evaluate() {
  unsigned=$(jq -c --argjson individual "$individual" --argjson corporate "$corporate" '
    ([$individual.signatures[].id] + [$corporate.agreements[].designated[] | select(.removed == null) | .id]) as $ids
    | map(select(.id as $id | $ids | index($id) | not))' <<<"$authors")
  flagged=$(jq -c --argjson corporate "$corporate" --argjson reviews "$reviews" --argjson pr "$PR" '
    [.[] | select(.company != null or (.domains | length > 0) or .when_agreeing)
     | .id as $id
     | . + {resolution: (
         if any($corporate.agreements[].designated[]; .id == $id and .removed == null) then "Corporate CLA"
         else ([$reviews.reviews[] | select(.kind == "employer" and .id == $id and .removed == null
                 and (.scope == "standing" or .pull_request == $pr))] | last
               | if . == null then null else .outcome + ", recorded by @" + .reviewed_by end)
         end)}]' <<<"$signals")
  unresolved=$(jq -c 'map(select(.resolution == null))' <<<"$flagged")
  unknown=$(jq -c --argjson reviews "$reviews" '
    [$reviews.reviews[] | select(.kind == "signed-offline" and .removed == null) | .email] as $offline
    | map(select(.email == "" or (.email as $e | $offline | index($e) | not)))' <<<"$unidentified")
}
evaluate

body=$(jq -rn --arg body "${COMMENT_BODY:-}" '$body | gsub("^\\s+|\\s+$"; "")')
commenter=$(lower "${COMMENTER_LOGIN:-}")
maintainer=$(jq -r --arg c "$commenter" 'if $c != "" and index($c) then "yes" else "no" end' <<<"$exempt")

# Someone who has to agree posts the statement.
if [[ "$body" == "$statement" ]] && jq -e --argjson id "$COMMENTER_ID" 'any(.id == $id)' <<<"$unsigned" >/dev/null; then
  record=$(jq -n --arg login "$COMMENTER_LOGIN" --argjson id "$COMMENTER_ID" --arg version "$version" \
    --arg sha256 "$({ sha256sum "$document" 2>/dev/null || shasum -a 256 "$document"; } | cut -d' ' -f1)" --arg at "$now" \
    --argjson pr "$PR" --arg comment "$COMMENT_URL" \
    --argjson signal "$(jq -c --argjson id "$COMMENTER_ID" '.[] | select(.id == $id) | {company, domains}' <<<"$signals")" \
    '{login: $login, id: $id, agreement: "individual", version: $version, document_sha256: $sha256,
      signed_at: $at, pull_request: $pr, comment: $comment,
      github_company: $signal.company, commit_email_domains: $signal.domains,
      employer_check: ($signal.company != null or ($signal.domains | length > 0))}')
  individual=$(append_record individual.json signatures "$record" "$COMMENTER_LOGIN agrees to the Individual CLA $version in #$PR")
  evaluate
fi

# A maintainer records the outcome of a review.
review=$(jq -cn --arg body "$body" '$body
  | (capture("^/cla employer @(?<login>[A-Za-z0-9-]+) (?<outcome>personal|authorized|authorized-standing):\\s*(?<note>\\S[\\s\\S]*)$") | . + {kind: "employer"})
    // (capture("^/cla signed-offline (?<email>[^\\s<>]+@[^\\s<>]+):\\s*(?<note>\\S[\\s\\S]*)$") | . + {kind: "signed-offline"})
    // null')
if [[ "$review" != null && "$maintainer" != yes ]]; then
  echo "ignored: only a maintainer records reviews" >&2
elif [[ "$review" != null ]]; then
  if [[ $(jq -r .kind <<<"$review") == employer ]]; then
    target=$(jq -c --argjson review "$review" \
      '[.[] | select((.login | ascii_downcase) == ($review.login | ascii_downcase))] | first // empty' <<<"$unresolved")
  else
    target=$(jq -c --argjson review "$review" \
      '[.[] | select(.email != "" and .email == ($review.email | ascii_downcase))] | first // empty' <<<"$unknown")
  fi
  if [[ -z "$target" ]]; then
    echo "ignored: the review names no one whose check is open on #$PR" >&2
  else
    record=$(jq -n --argjson review "$review" --argjson target "$target" --argjson pr "$PR" \
      --arg by "$COMMENTER_LOGIN" --argjson by_id "$COMMENTER_ID" --arg comment "$COMMENT_URL" --arg at "$now" '
      if $review.kind == "employer" then
        {kind: "employer", login: $target.login, id: $target.id,
         outcome: ($review.outcome | sub("-standing$"; "")),
         scope: (if $review.outcome == "authorized-standing" then "standing" else "pull_request" end),
         signs: {github_company: $target.company, commit_email_domains: $target.domains,
                 when_agreeing: $target.when_agreeing}}
      else
        {kind: "signed-offline", email: $target.email, name: $target.name, scope: "standing"}
      end
      + {pull_request: $pr, note: $review.note, reviewed_by: $by, reviewed_by_id: $by_id,
         comment: $comment, reviewed_at: $at}')
    reviews=$(append_record reviews.json reviews "$record" \
      "$COMMENTER_LOGIN records a review ($(jq -r '.kind + ": " + (.login // .email)' <<<"$record")) in #$PR")
    evaluate
  fi
fi

missing=$(jq length <<<"$unsigned")
orphans=$(jq length <<<"$unknown")
open=$(jq length <<<"$unresolved")
employers=$(jq length <<<"$flagged")
if ((missing + orphans + open > 0)); then
  state=failure
  description=$(jq -rn --argjson m "$missing" --argjson o "$orphans" --argjson e "$open" '
    [if $m > 0 then "waiting for \($m) author(s) to agree to the CLA" else empty end,
     if $o > 0 then "\($o) author(s) need manual review" else empty end,
     if $e > 0 then "employer authorization needs review for \($e) author(s)" else empty end]
    | join("; ") | (.[0:1] | ascii_upcase) + .[1:]')
elif [[ $(jq length <<<"$authors") == 0 ]]; then
  state=success
  description="No agreement needed: maintainers and bots only"
else
  state=success
  description="Every author has agreed to the CLA"
  ((employers > 0)) && description="$description; employer authorization reviewed"
fi
write -X POST "repos/$REPO/statuses/$head" -f state="$state" -f context=cla \
  -f description="${description:0:140}" -f target_url="$readme" >/dev/null
echo "cla: $state ($description)"

# One comment, kept up to date, once someone outside the exempt accounts is involved.
existing=$(gh api --paginate "repos/$REPO/issues/$PR/comments" \
  --jq ".[] | select(.user.login == \"github-actions[bot]\" and (.body | contains(\"$marker\"))) | .id" | head -n1)
if ((employers > 0)); then
  write "repos/$REPO/issues/$PR/labels" -f "labels[]=$label" >/dev/null || echo "could not add the label" >&2
fi
if [[ $state == success && -z "$existing" ]] && ((employers == 0)); then
  exit 0
fi

# Text from the pull request is quoted in code spans, without backticks or control characters.
# shellcheck disable=SC2016 # a jq definition
quote='def quote: tostring | gsub("[`[:cntrl:]]"; " ") | .[0:80] | "`" + . + "`";'
mention() { jq -r 'map("@" + .login) | join(", ")' <<<"$1"; }
if ((missing > 0)); then
  cla_line="waiting for $(mention "$unsigned")"
elif [[ $(jq length <<<"$authors") == 0 ]]; then
  cla_line="no agreement needed"
else
  cla_line="every author has agreed"
fi
((orphans > 0)) && cla_line="$cla_line; **manual review** for $orphans author(s)"
if ((employers == 0)); then
  employer_line="no sign of an employer"
else
  employer_line=$(jq -r '
    ([.[] | select(.resolution == null) | "@" + .login] | if length > 0 then ["**needs review**: " + join(", ")] else [] end)
    + ([.[] | select(.resolution != null) | "@" + .login + " (" + .resolution + ")"] | if length > 0 then ["reviewed: " + join(", ")] else [] end)
    | join("; ")' <<<"$flagged")
fi
message="$marker
| Check | Result |
|---|---|
| [Contributor License Agreement]($readme) | $cla_line |
| [Employer authorization]($readme#employer-check) | $employer_line |"

if ((missing > 0)); then
  if ((missing == 1)); then
    ask="$(mention "$unsigned") still needs to agree to the [Individual CLA](https://github.com/$REPO/blob/main/$document), by posting this comment exactly as written:"
  else
    ask="$(mention "$unsigned") still need to agree to the [Individual CLA](https://github.com/$REPO/blob/main/$document). Each of them can agree by posting this comment exactly as written:"
  fi
  message="$message

Thank you for your pull request. hidane accepts contributions under its [Contributor License Agreement]($readme): you keep the copyright in your contribution, and the project receives a license to use, distribute, sublicense and relicense it. Everyone who wrote the commits counts, including the people named in \`Co-authored-by:\` trailers.

$ask

\`\`\`
$statement
\`\`\`

If you contribute as part of your work for a company, it may need to sign the Corporate CLA instead; see the link above."
fi
if ((orphans > 0)); then
  message="$message

**CLA verification requires manual review** for these authors, because their email address is linked to no GitHub account:

$(jq -r "$quote"' map("- " + (.name | quote) + " " + (if .email == "" then "(no email address)" else ("<" + .email + ">" | quote) end) + ", " + .role + " of " + .sha) | join("\n")' <<<"$unknown")

Author commits with an email address linked to your GitHub account, and name co-authors with theirs (an email address verified on their account, or their \`users.noreply.github.com\` address); remove a \`Co-authored-by:\` trailer if that person did not write part of the commit. If someone cannot use GitHub, a maintainer can arrange another way to agree."
fi
if ((open > 0)); then
  message="$message

**Employer authorization requires review.** $(jq -r "$quote"' map("@" + .login + " (" + ([if .company then "profile: " + (.company | quote) else empty end] + [if (.domains | length) > 0 then "email: " + (.domains | map(quote) | join(", ")) else empty end] + [if .when_agreeing then "noted when agreeing to the CLA" else empty end] | join("; ")) + ")") | join(", ")' <<<"$unresolved") may be writing for a company. A company email or profile does not mean the company owns the contribution, but an employment contract can give it rights in what an employee writes, and the Individual CLA cannot grant those. Please tell us in a comment which applies:

- the contribution is your own: you wrote it outside any work for the company, and the company has no rights in it;
- the company has allowed you to contribute it under the Individual CLA (a maintainer will ask for its confirmation);
- the company signs the [Corporate CLA](https://github.com/$REPO/blob/main/docs/cla/corporate.md) and names you.

A maintainer records the outcome, and the check then passes."
fi
if [[ $state == failure ]]; then
  message="$message

Post \`recheck\` to run the check again."
fi
if [[ -n "$existing" ]]; then
  write -X PATCH "repos/$REPO/issues/comments/$existing" -f body="$message" >/dev/null
else
  write "repos/$REPO/issues/$PR/comments" -f body="$message" >/dev/null
fi
