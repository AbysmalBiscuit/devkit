# Turns Grok's review object into a GitHub review payload. With $inline,
# each finding is an inline comment and the summary lists them; without
# it, the summary carries every finding in full, for when GitHub rejects
# a comment whose lines fall outside the diff.

def priority: .priority // 3;

def badge:
  "<sub><sub>![P\(.) Badge](https://img.shields.io/badge/P\(.)-"
  + (["red", "orange", "yellow", "lightgrey"][.]) + "?style=flat)</sub></sub>";

def multiline: (.start_line // .line) < .line;

def location:
  (if multiline then "L\(.start_line)-L\(.line)" else "L\(.line)" end) as $lines
  | "[`\(.path)#\($lines)`](https://github.com/\($repo)/blob/\($sha)/\(.path)#\($lines))";

def heading: "**\(priority | badge)  \(.title)**";

def suggestion:
  if (.suggestion // "") == "" then ""
  else "\n\n```suggestion\n\(.suggestion | rtrimstr("\n"))\n```" end;

def comment:
  {path, line, side: "RIGHT", body: "\(heading)\n\n\(.body)\(suggestion)"}
  + if multiline then {start_line, start_side: "RIGHT"} else {} end;

def detail: "\(heading)\n\n\(location)\n\n\(.body)\(suggestion)";

def count($word): "\(length) \($word)\(if length == 1 then "" else "s" end)";

def quote: "> " + (rtrimstr("\n") | gsub("\n"; "\n> "));

(.findings | sort_by(priority)) as $findings
| {commit_id: $sha,
   event: (if any($findings[]; priority <= 1) then "REQUEST_CHANGES" else "COMMENT" end),
   body: ([
     "### ⚡ Grok Review",
     "**Reviewed commit:** `\($sha[:10])`",
     (if $findings == [] then "✅ **No issues found.**"
      elif .verdict == "correct" then "✅ **Patch is correct**, with \($findings | count("suggestion"))."
      else "❌ **Patch is incorrect**: \($findings | count("finding"))." end),
     (.explanation | quote),
     (if $findings == [] then empty
      elif $inline then
        ([$findings[] | "- \(priority | badge) \(.title) (\(location))"] | join("\n"))
      else
        ([$findings[] | detail] | join("\n\n---\n\n"))
      end)
   ] | join("\n\n")),
   comments: (if $inline then [$findings[] | comment] else [] end)}
