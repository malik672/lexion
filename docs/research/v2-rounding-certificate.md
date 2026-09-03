# Certified rounding envelope for 1-2 hop Uniswap V2 paths

This note freezes the proof boundary behind `V2PropagatedRoundingAuditV1`.

The current routing experiments use paths of at most two hops. For that search language, the path-specific propagated-rounding envelope can be justified directly. This note deliberately does **not** claim the same recurrence for arbitrary path length.

## One-hop response

For one Uniswap V2 hop, write the continuous no-floor response as

\[
q(x)=\frac{a x}{b+c x},\qquad a,b,c>0.
\]

The exact integer quote is

\[
Q(x)=\lfloor q(x)\rfloor.
\]

Therefore

\[
0\le q(x)-Q(x)<1.
\]

So a one-hop path has the certified raw-output error envelope

\[
E_1=1.
\]

## Concavity and the one-unit marginal bound

The continuous V2 response is increasing and concave:

\[
q'(x)=\frac{ab}{(b+cx)^2}>0,
\]

\[
q''(x)=-\frac{2abc}{(b+cx)^3}<0.
\]

For any increasing concave function with \(q(0)=0\), the gain from adding one unit decreases with the base point:

\[
q(z+1)-q(z)\le q(1)-q(0)=q(1).
\]

This is the key relationship that converts one raw unit of intermediate rounding into a final-output bound.

## Two-hop theorem

Let the ideal two-hop path be

\[
P^*(x)=q_2(q_1(x)),
\]

and exact integer replay be

\[
P(x)=\left\lfloor q_2\left(\left\lfloor q_1(x)\right\rfloor\right)\right\rfloor.
\]

Set

\[
m=\lfloor q_1(x)\rfloor.
\]

Then

\[
0\le q_1(x)-m<1.
\]

Decompose the total error:

\[
P^*(x)-P(x)
=
\underbrace{q_2(q_1(x))-q_2(m)}_{\text{propagated first-hop floor}}
+
\underbrace{q_2(m)-\lfloor q_2(m)\rfloor}_{\text{second-hop floor}}.
\]

Because \(q_2\) is increasing and concave,

\[
q_2(q_1(x))-q_2(m)
< q_2(m+1)-q_2(m)
\le q_2(1).
\]

And

\[
q_2(m)-\lfloor q_2(m)\rfloor<1.
\]

Therefore

\[
\boxed{0\le P^*(x)-P(x)<q_2(1)+1.}
\]

The exact simulator's one-unit suffix quote is

\[
Q_2(1)=\lfloor q_2(1)\rfloor.
\]

Since

\[
q_2(1)<Q_2(1)+1,
\]

we obtain a purely integer certificate:

\[
\boxed{P^*(x)-P(x)<Q_2(1)+2.}
\]

Thus define

\[
E(P)=
\begin{cases}
1,&|P|=1,\\
Q_{\text{suffix}}(1)+2,&|P|=2.
\end{cases}
\]

This is exactly the quantity empirically exercised by the current propagated-rounding audit for one- and two-hop V2 paths.

## Pairwise split objective

For two pool-disjoint V2 paths \(A\) and \(B\) splitting total input \(T\), define

\[
F^*(x)=P_A^*(T-x)+P_B^*(x)
\]

and exact integer replay

\[
F(x)=P_A(T-x)+P_B(x).
\]

Each continuous path response is increasing and concave, so \(F^*\) is concave on \([0,T]\).

Using the path envelopes,

\[
0\le F^*(x)-F(x)<E(A)+E(B).
\]

Hence any sound upper bound \(U_I\) on the continuous objective over an interval \(I\subseteq[0,T]\),

\[
F^*(x)\le U_I\qquad\forall x\in I,
\]

gives a sound upper bound on exact replay:

\[
F(x)<U_I+E(A)+E(B).
\]

Therefore, for an incumbent exact output \(B_{\mathrm{inc}}\), if

\[
\boxed{U_I+E(A)+E(B)\le B_{\mathrm{inc}},}
\]

then

\[
\boxed{\forall x\in I:\ F(x)\le B_{\mathrm{inc}}.}
\]

The entire allocation interval is provably dead and can be removed without exact replay at every concrete allocation.

## Empirical falsification status

The live-market `V2PropagatedRoundingAuditV1` found an adversarial V2-only order with 228 discrete concavity violations, maximum observed defect 401 raw output units, and maximum propagated pair envelope 815 raw units. The propagated-bound failure count was zero. The earlier constant and hop-count slack candidates failed on the same family.

This empirical result is not the proof; it is a differential falsifier for assumptions that may be missing from the mathematical model.

## Applicability boundary

The certificate in this note is valid only when all of the following hold:

1. every hop is an exact-input constant-product Uniswap V2-style response represented by \(q(x)=ax/(b+cx)\);
2. exact hop output is the floor of that rational response;
3. the path has one or two hops;
4. the two split paths are evaluated independently (in production, pool-disjointness is the sufficient condition currently used);
5. the continuous upper envelope used for pruning is itself sound.

In particular, this note makes no claim for V3, mixed paths, shared-pool portfolios, or arbitrary path length.

## Next production step

For a V2-only pair and allocation interval \(I=[\ell,h]\):

1. build the continuous composed CPMM response already used by the V2 allocator;
2. compute a sound upper bound \(U_I\) for
   \(F^*(x)=P_A^*(T-x)+P_B^*(x)\) using concavity (for example, exact continuous maximization or a supporting-line bound);
3. add the certified integer envelope \(E(A)+E(B)\);
4. prune only when the resulting upper bound cannot beat the incumbent exact replay.

This yields the first production candidate with the required one-sided property:

\[
\boxed{\text{pruned} \Rightarrow \text{provably unable to beat incumbent}.}
\]
