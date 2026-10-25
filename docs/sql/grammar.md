# ASQL grammar (BNF)

This is the grammar of ASQL, the SQL dialect of Accessor, as its parser accepts it. The notation is plain BNF with a few conveniences:

| Notation | Meaning |
| --- | --- |
| `<name>` | a non-terminal |
| `KEYWORD` | a keyword; keywords are case-insensitive |
| `'(' ')' ',' ';'` | a punctuation token |
| `a \| b` | either `a` or `b` |
| `[ a ]` | `a` is optional |
| `{ a }` | zero or more repetitions of `a` |
| `a , ...` | one or more `a` separated by commas (written out in the rules) |

A script is a sequence of statements separated by semicolons. Empty statements are ignored.

## Lexical elements

```bnf
<script>        ::= { ';' } [ <statement> { ';' { ';' } <statement> } ] { ';' }

<identifier>    ::= <plain-name> | '[' <any characters except ']'> ']'
<plain-name>    ::= <letter or '_'> { <letter> | <digit> | '_' }      (* not a reserved word *)
<parameter>     ::= '@' <plain-name>

<integer>       ::= <digit> { <digit> }                               (* INTEGER, or BIGNUM when too large *)
<decimal>       ::= <digit> { <digit> } '.' { <digit> }               (* DECIMAL *)
                  | '.' <digit> { <digit> }
<double>        ::= ( <integer> | <decimal> ) ( 'e' | 'E' ) [ '+' | '-' ] <digit> { <digit> }   (* DOUBLE *)
<string>        ::= "'" { <any character> | "''" } "'"
                  | '"' { <any character> | '""' } '"'                (* double quotes also make strings *)
<date-literal>  ::= '#' <date text> '#'
                  | ( DATE | TIMESTAMP | TIME ) <string>
                  | ( TIMESTAMP | TIME ) WITH TIME ZONE <string>
<boolean>       ::= TRUE | FALSE | YES | NO

<comment>       ::= '--' { <any character except newline> }
                  | '/*' { <any character> } '*/'
```

Identifiers and keywords are case-insensitive. An identifier that is a reserved word, or contains spaces or punctuation, is written in square brackets: `[Order Details]`, `[Group]`.

The reserved words are: `ALL AND AS ASC BETWEEN BY CASE CREATE CROSS DELETE DESC DISTINCT DROP ELSE END EXCEPT EXISTS FETCH FROM FULL GROUP HAVING IN INDEX INNER INSERT INTERSECT INTO IS JOIN LEFT LIKE LIMIT MOD NATURAL NOT NULL OFFSET ON OR ORDER OUTER PIVOT RIGHT SELECT SET TABLE THEN TOP UNION UNPIVOT UPDATE USING VALUES VIEW WHEN WHERE`. Other words that have a meaning in some place (`COUNT`, `YEAR`, `FIRST`, and so on) are not reserved and can be used as names.

## Statements

```bnf
<statement> ::= <query>
              | <explain> | <set-time-zone> | <set-constraints>
              | <create-sequence> | <alter-sequence> | <drop-sequence>
              | <prepare> | <deallocate>
              | <insert> | <update> | <delete> | <merge> | <truncate>
              | <create-table> | <create-index> | <create-view> | <create-trigger>
              | <drop> | <alter-table>
              | <parameters> | <exec>
              | <transaction-statement>
              | <raise>

<query>     ::= [ <with-clause> ] <query-body> [ <order-by> ] { <row-limit> }
              | <crosstab>
<crosstab>  ::= TRANSFORM <expr> [ AS <identifier> ]
                <select-core> [ <order-by> ] { <row-limit> }
                PIVOT <concat> [ IN '(' <expr-list> ')' ]
<explain>   ::= EXPLAIN [ QUERY PLAN ] <query>
<set-time-zone> ::= SET ( TIME ZONE | TIMEZONE TO ) ( <string> | LOCAL | UTC )
<prepare>     ::= PREPARE <identifier> AS <statement>
<deallocate>  ::= DEALLOCATE [ PREPARE ] ( <identifier> | ALL )     (* run with EXEC <identifier> [ '(' <exec-arg> { ',' <exec-arg> } ')' ] *)
<set-constraints> ::= SET CONSTRAINTS ( ALL | <identifier-list> ) ( DEFERRED | IMMEDIATE )

<create-sequence> ::= CREATE SEQUENCE [ IF NOT EXISTS ] <identifier> { <sequence-option> }
<alter-sequence>  ::= ALTER SEQUENCE [ IF EXISTS ] <identifier> { <sequence-option> }
<drop-sequence>   ::= DROP SEQUENCE [ IF EXISTS ] <identifier>
<sequence-option> ::= AS <identifier> | START [ WITH ] <integer> | INCREMENT [ BY ] <integer>
                    | MINVALUE <integer> | NO MINVALUE | MAXVALUE <integer> | NO MAXVALUE
                    | CYCLE | NO CYCLE | CACHE <integer> | RESTART [ WITH <integer> ]   (* RESTART only in ALTER *)
```

### Queries

```bnf
<with-clause>   ::= WITH [ RECURSIVE ] <cte> { ',' <cte> }
<cte>           ::= <identifier> [ '(' <identifier-list> ')' ] AS '(' <query> ')'

<query-body>    ::= <query-term> { <set-operator> [ ALL | DISTINCT ] <query-term> }
<set-operator>  ::= UNION | INTERSECT | EXCEPT | MINUS
<query-term>    ::= <select-core>
                  | <values>
                  | NEXT VALUE FOR <identifier> | PREVIOUS VALUE FOR <identifier>
                | <primary> ( '->' | '->>' ) <primary>                  (* JSON member or element *)
                | '(' <query> ')'

<values>        ::= VALUES <row> { ',' <row> }
<row>           ::= '(' <expr> { ',' <expr> } ')'

<select-core>   ::= SELECT { DISTINCT | ALL | TOP <integer> }
                    <select-item> { ',' <select-item> }
                    [ FROM <from-clause> ]
                    [ WHERE <expr> ]
                    [ GROUP BY <group-element> { ',' <group-element> } ]
                    [ HAVING <expr> ]
                    [ WINDOW <identifier> AS <window> { ',' <identifier> AS <window> } ]

<select-item>   ::= '*'
                  | <identifier> '.' '*'
                  | <expr> [ [ AS ] <identifier> ]

<group-element> ::= <expr>
                  | ROLLUP '(' <expr-list> ')'
                  | CUBE '(' <expr-list> ')'
                  | GROUPING SETS '(' <grouping-set> { ',' <grouping-set> } ')'
<grouping-set>  ::= '(' [ <expr-list> ] ')' | <expr>

<order-by>      ::= ORDER BY <order-item> { ',' <order-item> }
<order-item>    ::= <expr> [ ASC | DESC ] [ NULLS ( FIRST | LAST ) ]
<row-limit>     ::= LIMIT <integer>
                  | OFFSET <integer> [ ROW | ROWS ]
                  | FETCH ( FIRST | NEXT ) [ <integer> ] ( ROW | ROWS ) ONLY
```

`ORDER BY` and the row limits close the whole query: after a set operator they apply to the combined result.

```bnf
<from-clause>   ::= <from-source> { <join> }
<from-source>   ::= <table-factor>
                  | '(' <from-clause> ')'                  (* a parenthesised join, as Access writes it *)
<table-factor>  ::= <table-primary> { <pivot-clause> | <unpivot-clause> }
<table-primary> ::= <identifier> [ [ AS ] <identifier> ]
                  | [ LATERAL ] '(' <query> ')' [ AS ] <identifier> [ '(' <identifier-list> ')' ]
                  | [ LATERAL ] <identifier> '(' [ <expr-list> ] ')' [ [ AS ] <identifier> ] [ '(' <identifier-list> ')' ]
                                                            (* a table function: GENERATE_SERIES, SPLIT_STRING *)
<pivot-clause>  ::= PIVOT '(' <expr> FOR <concat> IN '(' <pivot-value> { ',' <pivot-value> } ')' ')' [ [ AS ] <identifier> ]
<pivot-value>   ::= <expr> [ AS ( <identifier> | <string> ) ]
<unpivot-clause> ::= UNPIVOT [ ( INCLUDE | EXCLUDE ) NULLS ]
                     '(' <identifier> FOR <identifier> IN '(' <unpivot-column> { ',' <unpivot-column> } ')' ')' [ [ AS ] <identifier> ]
<unpivot-column> ::= <identifier> [ AS ( <identifier> | <string> ) ]

<join>          ::= ',' <table-factor>
                  | CROSS JOIN <table-factor>
                  | [ NATURAL ] <join-type> JOIN <table-factor>          (* no condition after NATURAL *)
                  | <join-type> JOIN <table-factor> ( ON <expr> | USING '(' <identifier-list> ')' )
                  | JOIN <table-factor> ( ON <expr> | USING '(' <identifier-list> ')' )
<join-type>     ::= [ INNER ] | LEFT [ OUTER ] | RIGHT [ OUTER ] | FULL [ OUTER ]
```

### Data changes

```bnf
<insert>   ::= INSERT INTO <identifier> [ '(' <identifier-list> ')' ]
               ( VALUES <row> { ',' <row> }
               | DEFAULT VALUES
               | <query> )

<update>   ::= UPDATE <identifier> [ [ AS ] <identifier> ]
               SET <assignment> { ',' <assignment> }
               [ WHERE <expr> ]
<assignment> ::= [ <identifier> '.' ] <identifier> '=' <expr>

<delete>   ::= DELETE [ '*' ] FROM <identifier> [ [ AS ] <identifier> ] [ WHERE <expr> ]

<truncate> ::= TRUNCATE [ TABLE ] <identifier>

<merge>    ::= MERGE INTO <identifier> [ [ AS ] <identifier> ]
               USING <table-factor>
               ON <expr>
               <merge-when> { <merge-when> }
<merge-when> ::= WHEN MATCHED [ AND <expr> ] THEN ( UPDATE SET <assignment> { ',' <assignment> } | DELETE )
               | WHEN NOT MATCHED [ AND <expr> ] THEN
                 INSERT [ '(' <identifier-list> ')' ] VALUES <row>
```

### Definitions

```bnf
<create-table> ::= CREATE TABLE [ IF NOT EXISTS ] <identifier> '(' <table-element> { ',' <table-element> } ')'
<table-element> ::= <column-def>
                  | PRIMARY KEY '(' <identifier-list> ')'
                  | UNIQUE '(' <identifier-list> ')'
                  | [ CONSTRAINT <identifier> ] <check>
                  | [ CONSTRAINT <identifier> ] <foreign-key>

<column-def>   ::= <identifier> <data-type> { <column-constraint> }
<column-constraint> ::= NOT NULL | NULL
                      | PRIMARY KEY
                      | UNIQUE
                      | DEFAULT [ '-' ] <literal>
                      | [ CONSTRAINT <identifier> ] <check>
                      | REFERENCES <identifier> [ '(' <identifier-list> ')' ] { <referential-action> }
                      | AUTOINCREMENT | IDENTITY
                      | GENERATED ( ALWAYS | BY DEFAULT ) AS IDENTITY [ '(' <anything> ')' ]
                      | GENERATED ALWAYS AS '(' <expr> ')' [ STORED | VIRTUAL ]

<check>        ::= CHECK '(' <expr> ')'
<foreign-key>  ::= FOREIGN KEY '(' <identifier-list> ')'
                   REFERENCES <identifier> [ '(' <identifier-list> ')' ] { <referential-action> | <timing> }
<referential-action> ::= ON DELETE <action> | ON UPDATE <action>
<action>       ::= RESTRICT | CASCADE | SET NULL | SET DEFAULT | NO ACTION
<timing>       ::= [ NOT ] DEFERRABLE | INITIALLY ( DEFERRED | IMMEDIATE )

<data-type>    ::= <type-name> [ '(' <integer> [ ',' <integer> ] ')' ]     (* the sizes are accepted and ignored *)
                 | CALCULATED [ AS ] '(' <expr> ')'
<type-name>    ::= INTEGER | INT | LONG | SHORT | SMALLINT | BIGINT | BYTE | TINYINT
                 | COUNTER | AUTOINCREMENT | AUTONUMBER | SERIAL
                 | BIGNUM
                 | FLOAT | REAL | SINGLE
                 | DOUBLE [ PRECISION ]
                 | DECIMAL | NUMERIC | NUMBER | DEC
                 | CURRENCY | MONEY
                 | RATIONAL
                 | TEXT | VARCHAR | CHAR | CHARACTER [ VARYING ] | STRING | MEMO | LONGTEXT | NVARCHAR | NCHAR
                 | NATIONAL ( CHARACTER | CHAR ) [ VARYING ] | CLOB
                 | YESNO | BIT | BOOLEAN | BOOL | LOGICAL
                 | DATETIME | DATE | TIME | TIMESTAMP [ WITHOUT TIME ZONE ]
                 | TIMESTAMPTZ | ( TIMESTAMP | TIME ) WITH TIME ZONE
                 | INTERVAL | JSON | JSONB

<create-index> ::= CREATE [ UNIQUE ] INDEX [ IF NOT EXISTS ] <identifier> ON <identifier> '(' <identifier-list> ')'

<create-view>  ::= CREATE [ OR REPLACE ] ( VIEW | QUERY ) [ IF NOT EXISTS ] <identifier>
                   [ '(' ( <parameter-decl> { ',' <parameter-decl> } | <identifier-list> ) ')' ]    (* parameters, or column names *)
                   AS ( [ PARAMETERS <parameter-decl-list> ';' ] <query> [ WITH CHECK OPTION ] | <exec> )
<parameters>   ::= PARAMETERS <parameter-decl> { ',' <parameter-decl> }
<parameter-decl> ::= <parameter> [ <type-name> ]

<exec>         ::= ( EXEC | EXECUTE | CALL ) <identifier>
                   [ '(' ] [ <argument> { ',' <argument> } ] [ ')' ]
<argument>     ::= [ <parameter> '=' ] <expr>

<create-trigger> ::= CREATE [ OR REPLACE ] TRIGGER <identifier> ( BEFORE | AFTER | INSTEAD OF ) <trigger-event> { OR <trigger-event> }
                     ON <identifier> [ FOR EACH ROW ] [ WHEN <expr> ]
                     BEGIN <trigger-statement> ';' { <trigger-statement> ';' } END
<trigger-event> ::= INSERT | DELETE | UPDATE [ OF <identifier-list> ]
<trigger-statement> ::= <insert> | <update> | <delete> | <query> | <raise>
<raise>        ::= RAISE <expr>

<drop>         ::= DROP TABLE [ IF EXISTS ] <identifier>
                 | DROP ( VIEW | QUERY ) [ IF EXISTS ] <identifier>
                 | DROP TRIGGER [ IF EXISTS ] <identifier>
                 | DROP INDEX [ IF EXISTS ] <identifier> ON <identifier>

<alter-table>  ::= ALTER TABLE <identifier> <alter-action>
<alter-action> ::= ADD [ COLUMN ] <column-def>
                 | ADD <foreign-key>
                 | ADD CONSTRAINT <identifier> ( <foreign-key> | <check> )
                 | ADD <check>
                 | DROP [ COLUMN ] <identifier>
                 | DROP CONSTRAINT <identifier>
                 | RENAME TO <identifier>
                 | RENAME [ COLUMN ] <identifier> TO <identifier>
                 | ALTER [ COLUMN ] <identifier> [ TYPE ] <data-type>
                 | ALTER [ COLUMN ] <identifier> ( SET | DROP ) DEFAULT [ [ '-' ] <literal> ]    (* the literal only after SET *)
                 | ALTER [ COLUMN ] <identifier> ( SET | DROP ) NOT NULL
```

### Transactions

```bnf
<transaction-statement> ::= BEGIN [ TRANSACTION | WORK ]
                          | COMMIT [ TRANSACTION | WORK ]
                          | ROLLBACK [ TRANSACTION | WORK ] [ TO [ SAVEPOINT ] <identifier> ]
                          | SAVEPOINT <identifier>
                          | RELEASE [ SAVEPOINT ] <identifier>
```

## Expressions

Precedence, from loosest to tightest binding: `OR`; `AND`; `NOT`; comparison and the predicates; `&` and `||` (text concatenation); `+ -`; `* / \ % MOD`; unary `+ -`; `^`.

```bnf
<expr>        ::= <or-expr>
<or-expr>     ::= <and-expr> { OR <and-expr> }
<and-expr>    ::= <not-expr> { AND <not-expr> }
<not-expr>    ::= NOT <not-expr> | <comparison>

<comparison>  ::= <concat> { <comparison-tail> }
<comparison-tail> ::=
                    <compare-op> <concat>
                  | <compare-op> ( ANY | SOME | ALL ) '(' <query> ')'
                  | IS [ NOT ] NULL
                  | IS [ NOT ] ( TRUE | FALSE | UNKNOWN )
                  | IS [ NOT ] DISTINCT FROM <concat>
                  | [ NOT ] LIKE <concat> [ ESCAPE <concat> ]
                  | [ NOT ] BETWEEN [ SYMMETRIC | ASYMMETRIC ] <concat> AND <concat>
                  | [ NOT ] IN '(' <query> ')'
                  | [ NOT ] IN '(' <expr-list> ')'
<compare-op>  ::= '=' | '<>' | '!=' | '<' | '<=' | '>' | '>='

<concat>      ::= <additive> { ( '&' | '||' ) <additive> }
<additive>    ::= <multiplicative> { ( '+' | '-' ) <multiplicative> }
<multiplicative> ::= <unary> { ( '*' | '/' | '\' | '%' | MOD ) <unary> }
<unary>       ::= ( '-' | '+' ) <unary> | <power>
<power>       ::= <at-zone> [ '^' <unary> ]
<at-zone>     ::= <primary> { AT TIME ZONE <primary> }

<primary>     ::= <literal>
                | <parameter>
                | <column-ref>
                | '(' <expr> ')'
                | '(' <expr> ',' <expr-list> ')'                (* a row value, only in comparisons and IN *)
                | ROW '(' <expr-list> ')'
                | '(' <query> ')'                              (* scalar sub-query *)
                | [ NOT ] EXISTS '(' <query> ')'
                | <case>
                | <cast> | <extract> | <position> | <substring> | <trim>
                | CURRENT_DATE | CURRENT_TIMESTAMP | CURRENT_TIME | LOCALTIMESTAMP | LOCALTIME
                | <function-call>

<literal>     ::= <integer> | <decimal> | <double> | <string> | <date-literal> | <interval-literal> | <boolean> | NULL
<interval-literal> ::= INTERVAL <string> [ <interval-field> [ TO <interval-field> ] ]
<interval-field>   ::= YEAR | MONTH | DAY | HOUR | MINUTE | SECOND
<column-ref>  ::= <identifier> [ '.' <identifier> ]
<expr-list>   ::= <expr> { ',' <expr> }
<identifier-list> ::= <identifier> { ',' <identifier> }

<case>        ::= CASE [ <expr> ] WHEN <expr> THEN <expr> { WHEN <expr> THEN <expr> } [ ELSE <expr> ] END
<cast>        ::= CAST '(' <expr> AS <type-name> [ '(' <integer> [ ',' <integer> ] ')' ] ')'
<extract>     ::= EXTRACT '(' <field> FROM <expr> ')'
<position>    ::= POSITION '(' <additive> IN <expr> ')'
<substring>   ::= SUBSTRING '(' <expr> FROM <expr> [ FOR <expr> ] ')'
<trim>        ::= TRIM '(' [ LEADING | TRAILING | BOTH ] [ <expr> ] FROM <expr> ')'
                | TRIM '(' <expr> ')'

<function-call> ::= <identifier> '(' [ '*' | [ DISTINCT ] <expr-list> ] ')'
                    [ FILTER '(' WHERE <expr> ')' ]
                    [ OVER <window> ]
<window>      ::= <identifier>                                   (* OVER w: a window named in the WINDOW clause *)
                | '(' [ <identifier> ] [ PARTITION BY <expr-list> ] [ ORDER BY <order-item> { ',' <order-item> } ] [ <frame> ] ')'
<frame>       ::= ( ROWS | RANGE ) ( <bound> | BETWEEN <bound> AND <bound> )
<bound>       ::= UNBOUNDED PRECEDING | UNBOUNDED FOLLOWING | CURRENT ROW
                | <integer> PRECEDING | <integer> FOLLOWING        (* ROWS: a row count *)
                | <constant> PRECEDING | <constant> FOLLOWING      (* RANGE: a number, or an interval for dates *)
```
