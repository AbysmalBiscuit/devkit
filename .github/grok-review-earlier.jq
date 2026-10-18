# Renders this reviewer's earlier findings on a PR as a Markdown list: each
# inline thread with its state and replies, then titles found only in
# review bodies. Our comments carry a grok-review marker; older ones are
# github-actions comments.
def ours: (.body | contains("<!-- grok-review -->")) or .author.login == "github-actions";
def title: split("\n")[0] | gsub("<[^>]*>|!\\[[^]]*\\]\\([^)]*\\)|\\*\\*"; "") | gsub("^\\s+|\\s+$"; "");
def oneline: gsub("<!--.*?-->"; "") | gsub("\\s+"; " ") | .[:400];
.data.repository.pullRequest as $pr
| [$pr.reviewThreads.nodes[] | select(.comments.nodes[0] | ours)] as $threads
| [$threads[] | .comments.nodes[0].body | title] as $threaded
| [$pr.reviews.nodes[] | select(.body | startswith("### ⚡ Grok Review"))
   | .body | split("\n")[] | select(test("P[0-3] Badge"))
   | gsub("\\(\\[`[^)]*\\)\\)?$"; "") | sub("^- "; "") | title
   | select(. as $t | $threaded | index($t) | not)] | unique as $loose
| ([$threads[]
    | "- \(if .isResolved then "resolved" elif .isOutdated then "outdated" else "open" end): `\(.path):\(.line // .originalLine)` \(.comments.nodes[0].body | title)"
      + ([.comments.nodes[1:][] | "\n  - reply by \(.author.login // "ghost"): \(.body | oneline)"] | join(""))]
   + [$loose[] | "- in a review body: \(.)"])
| if . == [] then "None." else join("\n") end
