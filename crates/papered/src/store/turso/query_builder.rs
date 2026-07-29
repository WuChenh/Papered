use turso::Value;

/// A single filter condition: either a simple `column = ?N` equality or a raw
/// SQL fragment carrying its own `?` placeholders (renumbered in sequence).
#[derive(Debug, Clone)]
enum FilterClause {
    Eq(String, Value),
    Raw(String, Vec<Value>),
}

/// Helper for building parameterized `WHERE` clauses for Turso queries.
///
/// Example:
/// ```ignore
/// let mut filter = SqlFilter::new();
/// filter.eq("status", "indexed");
/// filter.eq("paper_type", "article");
///
/// let params: Vec<Value> = filter.params();
/// let where_sql = filter.where_clause(0); // "WHERE status = ?1 AND paper_type = ?2"
/// ```
#[derive(Debug, Default)]
pub struct SqlFilter(Vec<FilterClause>);

impl SqlFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Add an equality condition `column = ?N`.
    pub fn eq(&mut self, column: impl Into<String>, value: impl Into<Value>) {
        self.0.push(FilterClause::Eq(column.into(), value.into()));
    }

    /// Add a raw condition fragment whose `?` markers are renumbered in
    /// sequence with the other filters, binding `params` in order.
    pub fn raw(&mut self, sql: impl Into<String>, params: Vec<Value>) {
        self.0.push(FilterClause::Raw(sql.into(), params));
    }

    /// Return the parameter values in order.
    pub fn params(&self) -> Vec<Value> {
        self.0
            .iter()
            .flat_map(|clause| match clause {
                FilterClause::Eq(_, value) => vec![value.clone()],
                FilterClause::Raw(_, values) => values.clone(),
            })
            .collect()
    }

    /// Return the condition clauses (without `WHERE`).
    /// `offset` is the number of parameters already bound before these filters,
    /// so the first filter binds to `?{offset + 1}`.
    pub fn clauses(&self, offset: usize) -> Vec<String> {
        let mut next = offset + 1;
        let mut out = Vec::with_capacity(self.0.len());
        for clause in &self.0 {
            match clause {
                FilterClause::Eq(column, _) => {
                    out.push(format!("{column} = ?{next}"));
                    next += 1;
                }
                FilterClause::Raw(sql, params) => {
                    let mut rendered = String::with_capacity(sql.len() + 4);
                    for ch in sql.chars() {
                        if ch == '?' {
                            rendered.push_str(&format!("?{next}"));
                            next += 1;
                        } else {
                            rendered.push(ch);
                        }
                    }
                    debug_assert_eq!(
                        params.len(),
                        sql.matches('?').count(),
                        "raw filter placeholder count must match params"
                    );
                    out.push(rendered);
                }
            }
        }
        out
    }

    /// Return a `WHERE` clause or an empty string if there are no filters.
    pub fn where_clause(&self, offset: usize) -> String {
        let clauses = self.clauses(offset);
        if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        }
    }
}

/// Maximum host parameters per statement. SQLite caps at 999; 900 leaves headroom.
pub(crate) const MAX_QUERY_VARS: usize = 900;

/// `n` comma-separated anonymous placeholders (`"?,?,?"`) for `IN (...)` clauses.
pub(crate) fn placeholders(n: usize) -> String {
    (0..n).map(|_| "?").collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_produces_empty_where() {
        let filter = SqlFilter::new();
        assert!(filter.is_empty());
        assert_eq!(filter.where_clause(0), "");
    }

    #[test]
    fn filter_clauses_start_at_offset() {
        let mut filter = SqlFilter::new();
        filter.eq("status", Value::Text("indexed".to_string()));
        filter.eq("paper_type", Value::Text("article".to_string()));

        assert_eq!(
            filter.clauses(0),
            vec!["status = ?1".to_string(), "paper_type = ?2".to_string()]
        );
        assert_eq!(
            filter.clauses(2),
            vec!["status = ?3".to_string(), "paper_type = ?4".to_string()]
        );
    }

    #[test]
    fn where_clause_includes_where_keyword() {
        let mut filter = SqlFilter::new();
        filter.eq("content_type", Value::Text("text".to_string()));
        assert_eq!(filter.where_clause(0), "WHERE content_type = ?1");
    }

    #[test]
    fn raw_clauses_renumber_placeholders_in_sequence() {
        let mut filter = SqlFilter::new();
        filter.eq("status", Value::Text("indexed".to_string()));
        filter.raw(
            "EXISTS (SELECT 1 FROM t WHERE t.a = ? AND t.b = ?)",
            vec![Value::Text("x".to_string()), Value::Text("y".to_string())],
        );
        filter.eq("paper_type", Value::Text("review".to_string()));

        assert_eq!(
            filter.clauses(0),
            vec![
                "status = ?1".to_string(),
                "EXISTS (SELECT 1 FROM t WHERE t.a = ?2 AND t.b = ?3)".to_string(),
                "paper_type = ?4".to_string(),
            ]
        );
        assert_eq!(filter.params().len(), 4);
    }
}
