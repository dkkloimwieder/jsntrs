"""Extract the single-call fn:format-number expectations from the W3C QT3 suite."""
import xml.etree.ElementTree as ET
import re
import json
import sys
import collections

NS = '{http://www.w3.org/2010/09/qt-fots-catalog}'
root = ET.parse(sys.argv[1]).getroot()

PROP = r'[a-zA-Z-]+'
PROPS = r'((?:\s*' + PROP + r'\s*=\s*(?:"[^"]*"|\'[^\']*\')\s*)+);'
DECL_DEFAULT_RE = re.compile(r'declare\s+default\s+decimal-format\s+' + PROPS, re.S)
DECL_NAMED_RE = re.compile(r'declare\s+decimal-format\s+([A-Za-z][\w:-]*)\s+' + PROPS, re.S)
PROPPAIR_RE = re.compile(r'(' + PROP + r')\s*=\s*(?:"([^"]*)"|\'([^\']*)\')')


def split_args(s):
    out, depth, cur, q = [], 0, '', None
    for ch in s:
        if q:
            cur += ch
            if ch == q:
                q = None
            continue
        if ch in '"\'':
            q = ch
            cur += ch
            continue
        if ch == '(':
            depth += 1
        elif ch == ')':
            depth -= 1
        if ch == ',' and depth == 0:
            out.append(cur.strip())
            cur = ''
        else:
            cur += ch
    if cur.strip():
        out.append(cur.strip())
    return out


def unquote(a):
    a = a.strip()
    if len(a) >= 2 and a[0] == a[-1] and a[0] in '"\'':
        return a[1:-1].replace(a[0] * 2, a[0]), True
    return a, False


records, rejected = [], []
by_name = {}
for c in root.findall(NS + 'test-case'):
    name = c.get('name')
    by_name[name] = c
    q = (c.find(NS + 'test').text or '').strip()

    formats = {}      # decimal-format name ('' = default) -> properties
    for env in c.findall(NS + 'environment'):
        for df in env.findall(NS + 'decimal-format'):
            d = formats.setdefault(df.get('name') or '', {})
            for k, v in df.attrib.items():
                if k != 'name':
                    d[k] = v

    decls = [('', m.group(1)) for m in DECL_DEFAULT_RE.finditer(q)] + \
            [(m.group(1), m.group(2)) for m in DECL_NAMED_RE.finditer(q)]
    for key, props in decls:
        d = formats.setdefault(key, {})
        dup = False
        for pm in PROPPAIR_RE.finditer(props):
            k = pm.group(1)
            v = pm.group(2) if pm.group(2) is not None else pm.group(3)
            if k in d:
                dup = True
            d[k] = v
        if dup:
            d['__duplicate-property__'] = True

    body = DECL_NAMED_RE.sub('', DECL_DEFAULT_RE.sub('', q)).strip()
    body = re.sub(r'^let\s+\$x\s*:=\s*(.*?)\s+return\s+\$x$', r'\1', body, flags=re.S).strip()
    body = body.replace('fn:format-number', 'format-number')

    if not (body.startswith('format-number(') and body.endswith(')')
            and body.count('format-number(') == 1):
        rejected.append((name, 'not-a-single-call'))
        continue
    args = split_args(body[len('format-number('):-1])
    if len(args) not in (2, 3):
        rejected.append((name, 'arity-%d' % len(args)))
        continue
    picture, pic_lit = unquote(args[1])
    if not pic_lit:
        rejected.append((name, 'picture-not-a-literal'))
        continue

    rec = collections.OrderedDict()
    rec['qt3'] = name
    rec['value'] = args[0].strip()
    rec['picture'] = picture
    if len(args) == 3:
        fname, lit = unquote(args[2])
        if not lit:
            rejected.append((name, 'format-name-not-a-literal'))
            continue
        rec['decimal_format_name'] = fname
        if fname in formats:
            rec['decimal_format'] = formats[fname]
        else:
            rec['decimal_format_undeclared'] = True
    elif '' in formats:
        rec['decimal_format'] = formats['']

    r = c.find(NS + 'result')

    def one(el):
        tag = el.tag.replace(NS, '')
        if tag == 'assert-string-value':
            return {'string': el.text if el.text is not None else ''}
        if tag == 'error':
            return {'error': el.get('code')}
        return None

    kids = list(r)
    if len(kids) == 1 and kids[0].tag.replace(NS, '') == 'any-of':
        alts = [one(k) for k in kids[0]]
        if any(a is None for a in alts):
            rejected.append((name, 'unsupported-any-of'))
            continue
        rec['expect'] = {'any_of': alts}
    elif len(kids) == 1 and one(kids[0]) is not None:
        rec['expect'] = one(kids[0])
    else:
        rejected.append((name, 'unsupported-result'))
        continue

    deps = [{'type': d.get('type'), 'value': d.get('value')} for d in c.findall(NS + 'dependency')]
    if deps:
        rec['dependency'] = deps
    rec['query'] = ' '.join(q.split())
    records.append(rec)

json.dump(records, open(sys.argv[2], 'w'), indent=1, ensure_ascii=False)
print('kept', len(records), 'rejected', len(rejected))
print(collections.Counter(x[1] for x in rejected).most_common())
print('rejected ids:', [x[0] for x in rejected])

# --- JSON Lines output, natural-sorted by case id ---
def natkey(n):
    return [int(p) if p.isdigit() else p for p in re.split(r'(\d+)', n)]


records.sort(key=lambda r: natkey(r['qt3']))
with open(sys.argv[2] + 'l', 'w', encoding='utf-8') as fh:
    for r in records:
        fh.write(json.dumps(r, ensure_ascii=False, separators=(',', ':')) + '\n')

def render_result(r):
    kids = list(r)
    if len(kids) == 1 and kids[0].tag.replace(NS, '') == 'any-of':
        return {'any_of': [one(k) or {'assertion': k.tag.replace(NS, '')} for k in kids[0]]}
    if len(kids) == 1:
        return one(kids[0]) or {'assertion': kids[0].tag.replace(NS, '')}
    return {'assertions': [k.tag.replace(NS, '') for k in kids]}


with open(sys.argv[3], 'w', encoding='utf-8') as fh:
    for n, why in sorted(rejected, key=lambda x: natkey(x[0])):
        c = by_name[n]
        rec = collections.OrderedDict()
        rec['qt3'] = n
        rec['excluded'] = why
        rec['expect'] = render_result(c.find(NS + 'result'))
        rec['query'] = ' '.join((c.find(NS + 'test').text or '').split())
        fh.write(json.dumps(rec, ensure_ascii=False, separators=(',', ':')) + '\n')
