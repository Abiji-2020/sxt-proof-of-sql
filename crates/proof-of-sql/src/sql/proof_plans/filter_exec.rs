use super::{fold_columns, fold_vals};
use crate::{
    base::{
        database::{
            filter_util::filter_columns, Column, ColumnField, ColumnRef, LiteralValue, OwnedTable,
            Table, TableEvaluation, TableOptions, TableRef,
        },
        map::{IndexMap, IndexSet},
        proof::{PlaceholderResult, ProofError},
        scalar::Scalar,
        slice_ops,
    },
    sql::{
        proof::{
            FinalRoundBuilder, FirstRoundBuilder, HonestProver, ProofPlan, ProverEvaluate,
            ProverHonestyMarker, SumcheckSubpolynomialType, VerificationBuilder,
        },
        proof_exprs::{AliasedDynProofExpr, DynProofExpr, ProofExpr, TableExpr},
    },
    utils::log,
};
use alloc::{boxed::Box, vec, vec::Vec};
use bumpalo::Bump;
use core::marker::PhantomData;
use num_traits::{One, Zero};
use serde::{Deserialize, Serialize};
use sqlparser::ast::Ident;

/// Provable expressions for queries of the form
/// ```ignore
///     SELECT <result_expr1>, ..., <result_exprN> FROM <table> WHERE <where_clause>
/// ```
///
/// This differs from the [`FilterExec`] in that the result is not a sparse table.
#[derive(Debug, PartialEq, Serialize, Deserialize, Clone)]
pub struct OstensibleFilterExec<H: ProverHonestyMarker> {
    aliased_results: Vec<AliasedDynProofExpr>,
    table: TableExpr,
    /// TODO: add docs
    where_clause: DynProofExpr,
    phantom: PhantomData<H>,
}

impl<H: ProverHonestyMarker> OstensibleFilterExec<H> {
    /// Creates a new filter expression.
    pub fn new(
        aliased_results: Vec<AliasedDynProofExpr>,
        table: TableExpr,
        where_clause: DynProofExpr,
    ) -> Self {
        Self {
            aliased_results,
            table,
            where_clause,
            phantom: PhantomData,
        }
    }

    /// Get the aliased results
    pub fn aliased_results(&self) -> &[AliasedDynProofExpr] {
        &self.aliased_results
    }

    /// Get the table expression
    pub fn table(&self) -> &TableExpr {
        &self.table
    }

    /// Get the where clause expression
    pub fn where_clause(&self) -> &DynProofExpr {
        &self.where_clause
    }
}

impl<H: ProverHonestyMarker> ProofPlan for OstensibleFilterExec<H>
where
    OstensibleFilterExec<H>: ProverEvaluate,
{
    fn verifier_evaluate<S: Scalar>(
        &self,
        builder: &mut impl VerificationBuilder<S>,
        accessor: &IndexMap<TableRef, IndexMap<Ident, S>>,
        _result: Option<&OwnedTable<S>>,
        chi_eval_map: &IndexMap<TableRef, S>,
        params: &[LiteralValue],
    ) -> Result<TableEvaluation<S>, ProofError> {
        let input_chi_eval = *chi_eval_map
            .get(&self.table.table_ref)
            .expect("Chi eval not found");
        let accessor = accessor
            .get(&self.table.table_ref)
            .cloned()
            .unwrap_or_else(|| [].into_iter().collect());
        // 1. selection
        let selection_eval =
            self.where_clause
                .verifier_evaluate(builder, &accessor, input_chi_eval, params)?;
        // 2. columns
        let columns_evals = Vec::from_iter(
            self.aliased_results
                .iter()
                .map(|aliased_expr| {
                    aliased_expr
                        .expr
                        .verifier_evaluate(builder, &accessor, input_chi_eval, params)
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        // 3. filtered_columns
        let filtered_columns_evals =
            builder.try_consume_final_round_mle_evaluations(self.aliased_results.len())?;
        assert!(filtered_columns_evals.len() == self.aliased_results.len());

        let alpha = builder.try_consume_post_result_challenge()?;
        let beta = builder.try_consume_post_result_challenge()?;

        let output_chi_eval = builder.try_consume_chi_evaluation()?;

        verify_filter(
            builder,
            alpha,
            beta,
            input_chi_eval,
            output_chi_eval,
            &columns_evals,
            selection_eval,
            &filtered_columns_evals,
        )?;
        Ok(TableEvaluation::new(
            filtered_columns_evals,
            output_chi_eval,
        ))
    }

    fn get_column_result_fields(&self) -> Vec<ColumnField> {
        self.aliased_results
            .iter()
            .map(|aliased_expr| {
                ColumnField::new(aliased_expr.alias.clone(), aliased_expr.expr.data_type())
            })
            .collect()
    }

    fn get_column_references(&self) -> IndexSet<ColumnRef> {
        let mut columns = IndexSet::default();

        for aliased_expr in &self.aliased_results {
            aliased_expr.expr.get_column_references(&mut columns);
        }

        self.where_clause.get_column_references(&mut columns);

        columns
    }

    fn get_table_references(&self) -> IndexSet<TableRef> {
        IndexSet::from_iter([self.table.table_ref.clone()])
    }
}

/// Alias for a filter expression with a honest prover.
pub type FilterExec = OstensibleFilterExec<HonestProver>;

impl ProverEvaluate for FilterExec {
    #[tracing::instrument(name = "FilterExec::first_round_evaluate", level = "debug", skip_all)]
    fn first_round_evaluate<'a, S: Scalar>(
        &self,
        builder: &mut FirstRoundBuilder<'a, S>,
        alloc: &'a Bump,
        table_map: &IndexMap<TableRef, Table<'a, S>>,
        params: &[LiteralValue],
    ) -> PlaceholderResult<Table<'a, S>> {
        log::log_memory_usage("Start");

        let table = table_map
            .get(&self.table.table_ref)
            .expect("Table not found");
        // 1. selection
        let selection_column: Column<'a, S> = self
            .where_clause
            .first_round_evaluate(alloc, table, params)?;
        let selection = selection_column
            .as_boolean()
            .expect("selection is not boolean");
        let output_length = selection.iter().filter(|b| **b).count();

        // 2. columns
        let columns: Vec<_> = self
            .aliased_results
            .iter()
            .map(|aliased_expr| -> PlaceholderResult<Column<'a, S>> {
                aliased_expr.expr.first_round_evaluate(alloc, table, params)
            })
            .collect::<PlaceholderResult<Vec<_>>>()?;

        // Compute filtered_columns and indexes
        let (filtered_columns, _) = filter_columns(alloc, &columns, selection);
        let res = Table::<'a, S>::try_from_iter_with_options(
            self.aliased_results
                .iter()
                .map(|expr| expr.alias.clone())
                .zip(filtered_columns),
            TableOptions::new(Some(output_length)),
        )
        .expect("Failed to create table from iterator");
        builder.request_post_result_challenges(2);
        builder.produce_chi_evaluation_length(output_length);

        log::log_memory_usage("End");

        Ok(res)
    }

    #[tracing::instrument(name = "FilterExec::final_round_evaluate", level = "debug", skip_all)]
    fn final_round_evaluate<'a, S: Scalar>(
        &self,
        builder: &mut FinalRoundBuilder<'a, S>,
        alloc: &'a Bump,
        table_map: &IndexMap<TableRef, Table<'a, S>>,
        params: &[LiteralValue],
    ) -> PlaceholderResult<Table<'a, S>> {
        log::log_memory_usage("Start");

        let table = table_map
            .get(&self.table.table_ref)
            .expect("Table not found");
        // 1. selection
        let selection_column: Column<'a, S> = self
            .where_clause
            .final_round_evaluate(builder, alloc, table, params)?;
        let selection = selection_column
            .as_boolean()
            .expect("selection is not boolean");
        let output_length = selection.iter().filter(|b| **b).count();

        // 2. columns
        let columns: Vec<_> = self
            .aliased_results
            .iter()
            .map(|aliased_expr| -> PlaceholderResult<Column<'a, S>> {
                aliased_expr
                    .expr
                    .final_round_evaluate(builder, alloc, table, params)
            })
            .collect::<PlaceholderResult<Vec<_>>>()?;
        // Compute filtered_columns
        let (filtered_columns, result_len) = filter_columns(alloc, &columns, selection);
        // 3. Produce MLEs
        filtered_columns.iter().copied().for_each(|column| {
            builder.produce_intermediate_mle(column);
        });

        let alpha = builder.consume_post_result_challenge();
        let beta = builder.consume_post_result_challenge();

        prove_filter::<S>(
            builder,
            alloc,
            alpha,
            beta,
            &columns,
            selection,
            &filtered_columns,
            table.num_rows(),
            result_len,
        );
        let res = Table::<'a, S>::try_from_iter_with_options(
            self.aliased_results
                .iter()
                .map(|expr| expr.alias.clone())
                .zip(filtered_columns),
            TableOptions::new(Some(output_length)),
        )
        .expect("Failed to create table from iterator");

        log::log_memory_usage("End");

        Ok(res)
    }
}

#[expect(clippy::too_many_arguments, clippy::similar_names)]
pub(super) fn verify_filter<S: Scalar>(
    builder: &mut impl VerificationBuilder<S>,
    alpha: S,
    beta: S,
    chi_n_eval: S,
    chi_m_eval: S,
    c_evals: &[S],
    s_eval: S,
    d_evals: &[S],
) -> Result<(), ProofError> {
    let c_fold_eval = alpha * fold_vals(beta, c_evals);
    let d_fold_eval = alpha * fold_vals(beta, d_evals);
    let c_star_eval = builder.try_consume_final_round_mle_evaluation()?;
    let d_star_eval = builder.try_consume_final_round_mle_evaluation()?;

    // sum c_star * s - d_star = 0
    builder.try_produce_sumcheck_subpolynomial_evaluation(
        SumcheckSubpolynomialType::ZeroSum,
        c_star_eval * s_eval - d_star_eval,
        2,
    )?;

    // c_star + c_fold * c_star - chi_n = 0
    builder.try_produce_sumcheck_subpolynomial_evaluation(
        SumcheckSubpolynomialType::Identity,
        c_star_eval + c_fold_eval * c_star_eval - chi_n_eval,
        2,
    )?;

    // d_star + d_fold * d_star - chi_m = 0
    builder.try_produce_sumcheck_subpolynomial_evaluation(
        SumcheckSubpolynomialType::Identity,
        d_star_eval + d_fold_eval * d_star_eval - chi_m_eval,
        2,
    )?;

    // d_fold * chi_m - d_fold = 0
    builder.try_produce_sumcheck_subpolynomial_evaluation(
        SumcheckSubpolynomialType::Identity,
        d_fold_eval * (chi_m_eval - S::ONE),
        2,
    )?;

    Ok(())
}

#[expect(clippy::too_many_arguments, clippy::many_single_char_names)]
pub(super) fn prove_filter<'a, S: Scalar + 'a>(
    builder: &mut FinalRoundBuilder<'a, S>,
    alloc: &'a Bump,
    alpha: S,
    beta: S,
    c: &[Column<S>],
    s: &'a [bool],
    d: &[Column<S>],
    n: usize,
    m: usize,
) {
    let chi_n = alloc.alloc_slice_fill_copy(n, true);
    let chi_m = alloc.alloc_slice_fill_copy(m, true);

    let c_fold = alloc.alloc_slice_fill_copy(n, Zero::zero());
    fold_columns(c_fold, alpha, beta, c);
    let d_fold = alloc.alloc_slice_fill_copy(m, Zero::zero());
    fold_columns(d_fold, alpha, beta, d);

    let c_star = alloc.alloc_slice_copy(c_fold);
    slice_ops::add_const::<S, S>(c_star, One::one());
    slice_ops::batch_inversion(c_star);

    let d_star = alloc.alloc_slice_copy(d_fold);
    slice_ops::add_const::<S, S>(d_star, One::one());
    slice_ops::batch_inversion(d_star);

    builder.produce_intermediate_mle(c_star as &[_]);
    builder.produce_intermediate_mle(d_star as &[_]);

    // sum c_star * s - d_star = 0
    builder.produce_sumcheck_subpolynomial(
        SumcheckSubpolynomialType::ZeroSum,
        vec![
            (S::one(), vec![Box::new(c_star as &[_]), Box::new(s)]),
            (-S::one(), vec![Box::new(d_star as &[_])]),
        ],
    );

    // c_star + c_fold * c_star - chi_n = 0
    builder.produce_sumcheck_subpolynomial(
        SumcheckSubpolynomialType::Identity,
        vec![
            (S::one(), vec![Box::new(c_star as &[_])]),
            (
                S::one(),
                vec![Box::new(c_star as &[_]), Box::new(c_fold as &[_])],
            ),
            (-S::one(), vec![Box::new(chi_n as &[_])]),
        ],
    );

    // d_star + d_fold * d_star - chi_m = 0
    builder.produce_sumcheck_subpolynomial(
        SumcheckSubpolynomialType::Identity,
        vec![
            (S::one(), vec![Box::new(d_star as &[_])]),
            (
                S::one(),
                vec![Box::new(d_star as &[_]), Box::new(d_fold as &[_])],
            ),
            (-S::one(), vec![Box::new(chi_m as &[_])]),
        ],
    );

    // d_fold * chi_m - d_fold = 0
    builder.produce_sumcheck_subpolynomial(
        SumcheckSubpolynomialType::Identity,
        vec![
            (
                S::one(),
                vec![Box::new(d_fold as &[_]), Box::new(chi_m as &[_])],
            ),
            (-S::one(), vec![Box::new(d_fold as &[_])]),
        ],
    );
}
