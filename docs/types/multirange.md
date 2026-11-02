# The MULTIRANGE type

A multirange is a set of non-overlapping ranges, written the way PostgreSQL writes it: `{[1,3),[5,7)}`. The empty multirange is `{}`. It is a type of its own (`MULTIRANGE`, and the names `int4multirange`, `int8multirange`, `nummultirange`, `tsmultirange`, `tstzmultirange`, `datemultirange` and the multirange of any `CREATE TYPE r AS RANGE`), so `{}` is an empty multirange where a multirange is meant and an empty array where an array is meant; the two never mix.

* The ranges are kept sorted, and ranges that overlap or touch are joined.
* A multirange compares and sorts as the text it is written as, and as PostgreSQL orders them (an empty one first).
* A column of a multirange type puts what is stored in the form of its type (`int4multirange` writes `{[1,4)}` for `'{[1,3]}'`).
* Functions and operators are those of [ranges](../sql/dialects.md): `@>`, `<@`, `&&`, `<<`, `>>`, `-|-`, `+`, `-`, `*`, `lower`, `upper`, `isempty`, `range_agg`, `unnest` and the constructors, which also take `VARIADIC` an array of ranges.
* The graphical interface offers `MULTIRANGE` among the types of a field.
