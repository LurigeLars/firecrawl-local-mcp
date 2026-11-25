//! Repair detached numeric columns using the original OCR's line coordinates.

fn number(word: &str) -> bool {
    let word = word.trim_end_matches('*').trim_start_matches(['-', '+', '−']);
    word.starts_with(|c: char| c.is_ascii_digit())
        && word.ends_with(|c: char| c.is_ascii_digit())
        && word.bytes().all(|b| b.is_ascii_digit() || matches!(b, b',' | b'.'))
}

pub(super) fn has_detached_numbers(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(number)
        .scan(0, |run, numeric| {
            *run = if numeric { *run + 1 } else { 0 };
            Some(*run)
        })
        .any(|run| run >= 3)
}

struct Line<'a> {
    block: u32,
    left: u32,
    right: u32,
    top: u32,
    height: u32,
    words: Vec<&'a str>,
}

fn positions(tsv: &str) -> Option<Vec<Line<'_>>> {
    let mut rows = tsv.lines();
    if rows.next()? != "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext" {
        return None;
    }
    let mut lines: Vec<Line<'_>> = Vec::new();
    for row in rows {
        let fields: Vec<_> = row.splitn(12, '\t').collect();
        let [level, "1", block, _, _, _, left, top, width, height, _, word] = fields.as_slice() else {
            return None;
        };
        match *level {
            "4" => {
                let bounded = |s: &str| s.parse::<u32>().ok().filter(|n| *n <= 2200);
                let left = bounded(left)?;
                let top = bounded(top)?;
                let right = left + bounded(width)?;
                let height = bounded(height)?;
                if right > 2200 || top + height > 2200 || height == 0 || lines.len() >= 1024 {
                    return None;
                }
                lines.push(Line { block: block.parse().ok()?, left, right, top, height, words: Vec::new() });
            }
            "5" => lines.last_mut()?.words.extend(word.split_whitespace()),
            "1" | "2" | "3" => {}
            _ => return None,
        }
    }
    lines.retain(|line| !line.words.is_empty());
    Some(lines)
}

pub(super) fn restore_numeric_columns(text: &str, tsv: &str) -> Option<String> {
    let lines = positions(tsv)?;
    let original: Vec<_> = text.split_inclusive('\n').collect();
    let nonempty: Vec<_> = original.iter().enumerate().filter(|(_, line)| !line.trim().is_empty()).collect();
    // The coordinate pass must describe exactly the text already recognized.
    if lines.len() != nonempty.len() || lines.iter().zip(&nonempty).any(|(line, (_, text))|
        !line.words.iter().copied().eq(text.split_whitespace())) {
        return None;
    }

    let mut columns = vec![Vec::<&Line<'_>>::new(); lines.len()];
    let mut detached = vec![false; lines.len()];
    let labels: Vec<_> = lines.iter().enumerate().filter(|(_, line)| {
        line.words.iter().any(|word| number(word))
            && line.words.iter().any(|word| word.chars().any(char::is_alphabetic))
    }).collect();
    let mut start = 0;
    for block in lines.chunk_by(|a, b| a.block == b.block) {
        let first = start;
        start += block.len();
        if block.len() < 3 || block.iter().any(|line| {
            line.words.len() != 1 || !number(line.words[0])
                || line.right.abs_diff(block[0].right) > line.height / 2
        }) {
            continue;
        }
        let mut targets = Vec::<usize>::new();
        for line in block {
            let mut matches = labels.iter().filter(|&&(i, left)| {
                i < first && left.right + left.height / 2 <= line.left
                    && (2 * left.top + left.height).abs_diff(2 * line.top + line.height)
                        <= left.height.min(line.height)
            });
            let Some(&(target, left)) = matches.next() else { break };
            if matches.next().is_some()
                || targets.last().is_some_and(|previous| {
                    target != previous + 1 || left.block != lines[*previous].block
                })
                || columns[target].iter().any(|other| other.left < line.right && line.left < other.right) {
                break;
            }
            targets.push(target);
        }
        if targets.len() == block.len() {
            for (target, line) in targets.into_iter().zip(block) {
                columns[target].push(line);
            }
            detached[first..start].fill(true);
        }
    }

    if !detached.contains(&true) { return None; }
    let mut result = String::new();
    let mut index = 0;
    for line in original {
        if line.trim().is_empty() {
            result.push_str(line);
            continue;
        }
        if !detached[index] {
            let body = line.trim_end_matches(['\r', '\n']);
            result.push_str(body);
            columns[index].sort_unstable_by_key(|column| column.left);
            for column in &columns[index] {
                result.push(' ');
                result.push_str(column.words[0]);
            }
            result.push_str(&line[body.len()..]);
        }
        index += 1;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "A prose column.\n\nJanuary 12,100\nFebruary 12,200\nMarch 12,300\n\nA footnote.\n\n99,100\n99,200\n99,300\n";

    fn tsv(third_top: u32, ambiguous: bool) -> String {
        let mut tsv = String::from("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n");
        let mut add = |block, line, left, top, width, words: &str| {
            tsv.push_str(&format!("4\t1\t{block}\t1\t{line}\t0\t{left}\t{top}\t{width}\t20\t-1\t\n"));
            for (i, word) in words.split_whitespace().enumerate() {
                tsv.push_str(&format!("5\t1\t{block}\t1\t{line}\t{}\t{left}\t{top}\t10\t20\t95\t{word}\n", i + 1));
            }
        };
        add(1, 1, 40, 30, 200, "A prose column.");
        add(2, 1, 40, 100, 250, "January 12,100");
        add(2, 2, 40, 135, 250, "February 12,200");
        add(2, 3, 40, 170, 250, "March 12,300");
        if ambiguous {
            add(3, 1, 40, 100, 180, "Other 21,400");
        } else {
            add(3, 1, 40, 210, 180, "A footnote.");
        }
        add(4, 1, 360, 101, 70, "99,100");
        add(4, 2, 360, 136, 70, "99,200");
        add(4, 3, 360, third_top, 70, "99,300");
        tsv
    }

    #[test]
    fn restores_a_full_aligned_column_and_preserves_other_text() {
        assert!(has_detached_numbers(TEXT));
        assert_eq!(restore_numeric_columns(TEXT, &tsv(171, false)).unwrap(),
            "A prose column.\n\nJanuary 12,100 99,100\nFebruary 12,200 99,200\nMarch 12,300 99,300\n\nA footnote.\n\n");
    }

    #[test]
    fn keeps_original_when_any_pair_is_unconfirmed() {
        assert!(restore_numeric_columns(TEXT, &tsv(185, false)).is_none());
        assert!(restore_numeric_columns(&TEXT.replace("99,300", "99,900"), &tsv(171, false)).is_none());
        assert!(restore_numeric_columns(&TEXT.replace("A footnote.", "Other 21,400"), &tsv(171, true)).is_none());
        assert!(!has_detached_numbers("January 12,100\nFebruary 12,200\nMarch 12,300"));
    }
}
