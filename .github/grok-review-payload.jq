# Turns Grok's review object into a GitHub review payload. With $inline,
# each finding is an inline comment and the summary lists them; without
# it, the summary carries every finding in full, for when GitHub rejects
# the inline review anyway. Only a P0 finding requests changes.
#
# $files maps each cited tracked file to its lines, and $lines maps each
# diffed file's commentable lines to their hunk (grok-review-lines.awk).
# GitHub rejects the whole review when one comment falls outside the diff,
# so every comment is placed here first: by its quote, then into a hunk.

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

def trimmed: gsub("^\\s+|\\s+$"; "");

# Moves the finding's lines to the line holding its quote: the cited line
# when it does, else the nearest one that does. Null when the file is not
# tracked or the quote is not in it.
def located:
  . as $f
  | ($files[0][$f.path] // null) as $text
  | ($f.quote // "" | trimmed) as $q
  | ([$f.start_line // $f.line, $f.line] | min) as $start
  | if $text == null or $q == "" then null
    else [range(0; $text | length) | select($text[.] | contains($q)) | . + 1]
      | if . == [] then null
        else min_by(. - $f.line | fabs) as $at
          | $f + {line: $at, start_line: ($start + $at - $f.line)}
        end
    end;

# Places a located finding: on the part of its range inside the last hunk
# it overlaps, or at the file's nearest diff line, pointing at its real
# lines. Null when the file has no diff lines.
def anchored:
  . as $f
  | ($lines[0][$f.path] // {}) as $m
  | [$m | to_entries[] | {line: (.key | tonumber), hunk: .value}
      | select(.line >= $f.start_line and .line <= $f.line)] as $overlap
  | if $overlap != [] then
      ($overlap | max_by(.line) | .hunk) as $h
      | [$overlap[] | select(.hunk == $h) | .line] as $kept
      | $f + {start_line: ($kept | min), line: ($kept | max)}
        + if [$kept | min, max] != [$f.start_line, $f.line] then {suggestion: null} else {} end
    elif $m != {} then
      $f + {anchor: ([$m | keys[] | tonumber] | min_by(. - $f.line | fabs)), suggestion: null}
    else null end;

def comment:
  if .anchor then
    {path, line: .anchor, side: "RIGHT", body: "\(heading)\n\nAt \(location), outside the diff.\n\n\(.body)"}
  else
    {path, line, side: "RIGHT", body: "\(heading)\n\n\(.body)\(suggestion)"}
    + if multiline then {start_line, start_side: "RIGHT"} else {} end
  end
  | .body += "\n\n<!-- grok-review -->";

def detail: "\(heading)\n\n\(location)\n\n\(.body)\(suggestion)";

def count($word): "\(length) \($word)\(if length == 1 then "" else "s" end)";

def blockquote: "> " + (rtrimstr("\n") | gsub("\n"; "\n> "));

(.findings | sort_by(priority)) as $findings
| (if $inline then [$findings[] | {f: ., placed: (located | if . then anchored else null end)}]
   else [] end) as $placed
| [$placed[] | select(.placed) | .placed] as $comments
| [$placed[] | select(.placed | not) | .f] as $unplaced
| {commit_id: $sha,
   event: (if any($findings[]; priority == 0) then "REQUEST_CHANGES" else "COMMENT" end),
   body: ([
     "### ⚡ Grok Review",
     "**Reviewed commit:** `\($sha[:10])`",
     (if $findings == [] then "✅ **No issues found.**"
      elif .verdict == "correct" then "✅ **Patch is correct**, with \($findings | count("suggestion"))."
      else "❌ **Patch is incorrect**: \($findings | count("finding"))." end),
     (.explanation | blockquote),
     (if $findings == [] then empty
      elif $inline then
        ([$comments[] | "- \(priority | badge) \(.title) (\(location))"] | join("\n")),
        (if $unplaced == [] then empty
         else "#### Not in the diff\n\nThese have no inline thread: their quote is in no tracked file, or their file is not in the diff.\n\n"
           + ([$unplaced[] | detail] | join("\n\n---\n\n")) end)
      else
        ([$findings[] | detail] | join("\n\n---\n\n"))
      end)
   ] | map(select(. != "")) | join("\n\n")),
   comments: [$comments[] | comment]}
