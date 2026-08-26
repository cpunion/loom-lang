# Checkout composition fixture

Status: **Archived; non-normative**

This record defines the intended fairness and observability contract for the
archived [checkout composition study](checkout-composition-study.md). It never
became an executable fixture. All syntax below is conceptual.

## Domain contract

Both implementations would have represented unconstrained transport input and
validated domain values for carts, cart lines, money, customers, VAT identity,
operation identity, checkout requests, and orders.

One `CheckoutRequest.from(raw)` boundary would have validated errors in a fixed
order: empty cart, line quantities, unit prices, VAT identity, the EU VAT
presence invariant, and operation identity. Any failure would have become a
validation result before the checkout flow started, leaving the external-call
trace empty.

The conceptual processing order was:

```text
validate input
-> calculate base price
-> apply pricing transforms
-> calculate tax
-> run pre-authorization checks
-> authorize
-> handle a business rejection
-> persist once by operation id
-> return the order
```

Three predeclared extension points were proposed:

| Extension point | Typed transform | Allowed external dependency |
| --- | --- | --- |
| Pricing | `PricedCart -> Result[PricedCart, PricingError]` | none |
| Pre-authorization | `AuthorizationContext -> Result[AuthorizationContext, PreAuthorizationError]` | risk service |
| Rejection handling | `AuthorizationRejectedContext -> Result[AuthorizationRejectedContext, RejectionHandlingError]` | audit sink |

Each point would have used the ordered-pipeline algebra described in the
[static-composition archive](static-composition.md): an empty identity,
slot-local unique keys, explicit ordering, first-error termination, and no
ability to widen the owner's failures or dependency bounds.

## Deterministic providers

The fixture would have supplied fresh, in-memory providers for authorization,
risk, tax, order storage, and audit. A shared per-scenario recorder would have
assigned monotonically increasing event indices at call boundaries so that
cross-provider ordering could be tested directly. Provider descriptors,
adapters, and the trace schema would have been versioned inputs rather than
hidden test-process conventions.

The error mapping would have distinguished validation, pricing, tax,
pre-authorization, provider unavailability, business rejection, rejection
handler failure, and persistence. A rejection-handler failure would have
retained both the original authorization reason and the handling error.

## Oracle layers

The planned fixture had four independent oracle layers:

1. **Behavior**: invalid inputs, customer and region variants, risk decisions,
   authorization outcomes, audit failure, persistence failure, and idempotent
   operation IDs.
2. **Order**: no external calls after validation failure; risk before
   authorization; audit only after a business rejection; persistence only after
   authorization; tax before risk and authorization.
3. **Explanation**: the stable identity, source, target, dependencies, ordering
   reason, activation path, and static removal impact of every rule.
4. **Mutation**: changes such as moving VAT validation after authorization,
   applying discounts after tax, losing rejection audits, conflating provider
   failure with rejection, persisting early, reversing ordering edges, or
   dropping an active contribution.

Pure pricing transforms would have been checked through the typed plan, results,
and mutations rather than being disguised as external capability events.

## Configuration matrix

Pricing would have covered an empty pipeline, product promotions only, VIP
discount only, and both transforms. The combined case would require the VIP
discount explicitly after product promotions. The full task configuration would
also activate the risk and rejection-audit contributions and bind all five
provider slots.

Imports would never activate a contribution. A target would have selected it by
qualified identity, making every active path traceable through target,
contribution, and owner-declared extension point.

## Acceptance conditions

Before implementation, the study required a domain expert to review all results
and orderings, an independent TypeScript reviewer to approve the baseline, a
frozen provider lifecycle and trace schema, an executable configuration matrix,
equivalent extension points, balanced task variants, and repeatable behavior,
trace, mutation, and explanation scoring.

Those prerequisites were never completed. The fixture remains a design artifact
and must not be used to infer current Loom features or future commitments.
