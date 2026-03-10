const CHINESE_CHAR_REGEX = /[\u3400-\u9fff]/u

type PinyinModule = typeof import('pinyin-pro')

let pinyinMod: PinyinModule | null = null
let pinyinLoading: Promise<void> | null = null

function loadPinyin(): Promise<void> {
  if (pinyinMod) return Promise.resolve()
  if (pinyinLoading) return pinyinLoading

  pinyinLoading = import('pinyin-pro')
    .then((mod) => {
      pinyinMod = mod
    })
    .catch(() => {
      pinyinLoading = null
    })
  return pinyinLoading
}

export function preloadPinyin(): void {
  loadPinyin()
}

function normalizeSearchText(value: string): string {
  return value.normalize('NFKC').toLocaleLowerCase().replace(/\s+/g, ' ').trim()
}

function compactSearchText(value: string): string {
  return normalizeSearchText(value).replace(/\s+/g, '')
}

function addSearchTerm(terms: Set<string>, value: string): void {
  const normalizedValue = normalizeSearchText(value)
  if (!normalizedValue) {
    return
  }

  terms.add(normalizedValue)

  const compactValue = normalizedValue.replace(/\s+/g, '')
  if (compactValue && compactValue !== normalizedValue) {
    terms.add(compactValue)
  }
}

function getPinyinSearchTerms(value: string): string[] {
  if (!CHINESE_CHAR_REGEX.test(value) || !pinyinMod) {
    return []
  }

  const fullPinyin = pinyinMod.pinyin(value, {
    toneType: 'none',
    type: 'array',
  })
  const initials = pinyinMod.pinyin(value, {
    pattern: 'first',
    toneType: 'none',
    type: 'array',
  })

  return [
    fullPinyin.join(' '),
    fullPinyin.join(''),
    initials.join(' '),
    initials.join(''),
  ]
}

export function buildSearchTerms(
  ...values: Array<string | null | undefined>
): string[] {
  const terms = new Set<string>()

  for (const rawValue of values) {
    const value = rawValue?.trim()
    if (!value) {
      continue
    }

    addSearchTerm(terms, value)

    for (const pinyinTerm of getPinyinSearchTerms(value)) {
      addSearchTerm(terms, pinyinTerm)
    }
  }

  return Array.from(terms)
}

export function matchesSearchQuery(
  query: string,
  ...values: Array<string | null | undefined>
): boolean {
  const normalizedQuery = normalizeSearchText(query)
  if (!normalizedQuery) {
    return true
  }

  const compactQuery = compactSearchText(normalizedQuery)
  const queryVariants =
    compactQuery && compactQuery !== normalizedQuery
      ? [normalizedQuery, compactQuery]
      : [normalizedQuery]

  const searchTerms = buildSearchTerms(...values)
  return searchTerms.some((term) =>
    queryVariants.some((variant) => term.includes(variant)),
  )
}
