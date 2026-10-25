package com.accessor.jdbc;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.RowIdLifetime;
import java.sql.SQLException;
import java.sql.Statement;

/**
 * What a program or tool can ask about the database. The catalog queries are answered from
 * {@code INFORMATION_SCHEMA}. A database has one schema and no catalogs, so those arguments are
 * ignored and the schema and catalog columns of the results are NULL.
 */
final class AccessorDatabaseMetaData extends UnsupportedDatabaseMetaData {
    private final AccessorConnection conn;

    AccessorDatabaseMetaData(AccessorConnection conn) {
        this.conn = conn;
    }

    // ----- helpers ----------------------------------------------------------------------------

    private ResultSet query(String sql) throws SQLException {
        conn.checkOpen();
        Statement s = conn.createStatement(ResultSet.TYPE_SCROLL_INSENSITIVE, ResultSet.CONCUR_READ_ONLY);
        s.closeOnCompletion();
        return s.executeQuery(sql);
    }

    /** A text literal for SQL, or the match-everything pattern for null. */
    private static String pattern(String p) {
        return p == null ? "'%'" : "'" + p.replace("'", "''") + "'";
    }

    private static String literal(String s) {
        return "'" + s.replace("'", "''") + "'";
    }

    private ResultSet empty(String... columns) throws SQLException {
        StringBuilder sb = new StringBuilder("SELECT ");
        for (int i = 0; i < columns.length; i++) {
            sb.append(i > 0 ? ", " : "").append("NULL AS ").append(columns[i]);
        }
        return query(sb.append(" WHERE 1 = 0").toString());
    }

    /** The JDBC type code for an Accessor type name, as a SQL expression over {@code data_type}. */
    private static final String SQL_TYPE =
            "CASE data_type WHEN 'INTEGER' THEN -5 WHEN 'TEXT' THEN 12 WHEN 'DOUBLE' THEN 8 WHEN 'FLOAT' THEN 7 WHEN 'YESNO' THEN 16"
                    + " WHEN 'DECIMAL' THEN 3 WHEN 'CURRENCY' THEN 3 WHEN 'BIGNUM' THEN 2 WHEN 'DATETIME' THEN 93"
                    + " WHEN 'TIMESTAMPTZ' THEN 2014 ELSE 1111 END";

    private static String rule(String column) {
        return "CASE " + column + " WHEN 'CASCADE' THEN 0 WHEN 'RESTRICT' THEN 1 WHEN 'SET NULL' THEN 2 WHEN 'NO ACTION' THEN 3"
                + " WHEN 'SET DEFAULT' THEN 4 ELSE 3 END";
    }

    // ----- the catalog ------------------------------------------------------------------------

    @Override
    public ResultSet getTables(String catalog, String schemaPattern, String tableNamePattern, String[] types) throws SQLException {
        StringBuilder where = new StringBuilder("table_name LIKE " + pattern(tableNamePattern));
        if (types != null && types.length > 0) {
            where.append(" AND (CASE table_type WHEN 'VIEW' THEN 'VIEW' ELSE 'TABLE' END) IN (");
            for (int i = 0; i < types.length; i++) {
                where.append(i > 0 ? ", " : "").append(literal(types[i].toUpperCase()));
            }
            where.append(")");
        }
        return query("SELECT NULL AS TABLE_CAT, NULL AS TABLE_SCHEM, table_name AS TABLE_NAME,"
                + " CASE table_type WHEN 'VIEW' THEN 'VIEW' ELSE 'TABLE' END AS TABLE_TYPE, NULL AS REMARKS, NULL AS TYPE_CAT,"
                + " NULL AS TYPE_SCHEM, NULL AS TYPE_NAME, NULL AS SELF_REFERENCING_COL_NAME, NULL AS REF_GENERATION"
                + " FROM information_schema.tables WHERE " + where + " ORDER BY TABLE_TYPE, TABLE_NAME");
    }

    @Override
    public ResultSet getColumns(String catalog, String schemaPattern, String tableNamePattern, String columnNamePattern) throws SQLException {
        return query("SELECT NULL AS TABLE_CAT, NULL AS TABLE_SCHEM, table_name AS TABLE_NAME, column_name AS COLUMN_NAME,"
                + " " + SQL_TYPE + " AS DATA_TYPE, COALESCE(data_type, 'ANY') AS TYPE_NAME, NULL AS COLUMN_SIZE, NULL AS BUFFER_LENGTH,"
                + " NULL AS DECIMAL_DIGITS, 10 AS NUM_PREC_RADIX,"
                + " CASE WHEN is_nullable = 'NO' THEN 0 ELSE 1 END AS NULLABLE, NULL AS REMARKS, column_default AS COLUMN_DEF,"
                + " NULL AS SQL_DATA_TYPE, NULL AS SQL_DATETIME_SUB, NULL AS CHAR_OCTET_LENGTH, ordinal_position AS ORDINAL_POSITION,"
                + " COALESCE(is_nullable, 'YES') AS IS_NULLABLE, NULL AS SCOPE_CATALOG, NULL AS SCOPE_SCHEMA, NULL AS SCOPE_TABLE,"
                + " NULL AS SOURCE_DATA_TYPE, IIf(is_autonumber, 'YES', 'NO') AS IS_AUTOINCREMENT,"
                + " IIf(calculated_expression IS NULL, 'NO', 'YES') AS IS_GENERATEDCOLUMN"
                + " FROM information_schema.columns WHERE table_name LIKE " + pattern(tableNamePattern)
                + " AND column_name LIKE " + pattern(columnNamePattern) + " ORDER BY TABLE_NAME, ORDINAL_POSITION");
    }

    @Override
    public ResultSet getPrimaryKeys(String catalog, String schema, String table) throws SQLException {
        return query("SELECT NULL AS TABLE_CAT, NULL AS TABLE_SCHEM, k.table_name AS TABLE_NAME, k.column_name AS COLUMN_NAME,"
                + " k.ordinal_position AS KEY_SEQ, k.constraint_name AS PK_NAME"
                + " FROM information_schema.key_column_usage k JOIN information_schema.table_constraints c"
                + " ON c.constraint_name = k.constraint_name AND c.table_name = k.table_name"
                + " WHERE c.constraint_type = 'PRIMARY KEY' AND k.table_name LIKE " + pattern(table) + " ORDER BY TABLE_NAME, KEY_SEQ");
    }

    private ResultSet keys(String where) throws SQLException {
        return query("SELECT NULL AS PKTABLE_CAT, NULL AS PKTABLE_SCHEM, k.referenced_table AS PKTABLE_NAME, k.referenced_column AS PKCOLUMN_NAME,"
                + " NULL AS FKTABLE_CAT, NULL AS FKTABLE_SCHEM, k.table_name AS FKTABLE_NAME, k.column_name AS FKCOLUMN_NAME,"
                + " k.ordinal_position AS KEY_SEQ, " + rule("r.update_rule") + " AS UPDATE_RULE, " + rule("r.delete_rule") + " AS DELETE_RULE,"
                + " k.constraint_name AS FK_NAME, NULL AS PK_NAME,"
                + " CASE WHEN r.is_deferrable = 'NO' THEN 7 WHEN r.initially_deferred = 'YES' THEN 5 ELSE 6 END AS DEFERRABILITY"
                + " FROM information_schema.key_column_usage k JOIN information_schema.referential_constraints r"
                + " ON r.constraint_name = k.constraint_name AND r.table_name = k.table_name"
                + " WHERE k.referenced_table IS NOT NULL AND " + where + " ORDER BY PKTABLE_NAME, FKTABLE_NAME, FK_NAME, KEY_SEQ");
    }

    @Override
    public ResultSet getImportedKeys(String catalog, String schema, String table) throws SQLException {
        return keys("k.table_name LIKE " + pattern(table));
    }

    @Override
    public ResultSet getExportedKeys(String catalog, String schema, String table) throws SQLException {
        return keys("k.referenced_table LIKE " + pattern(table));
    }

    @Override
    public ResultSet getCrossReference(String pc, String ps, String parent, String fc, String fs, String foreign) throws SQLException {
        return keys("k.referenced_table LIKE " + pattern(parent) + " AND k.table_name LIKE " + pattern(foreign));
    }

    @Override
    public ResultSet getIndexInfo(String catalog, String schema, String table, boolean unique, boolean approximate) throws SQLException {
        return query("SELECT NULL AS TABLE_CAT, NULL AS TABLE_SCHEM, table_name AS TABLE_NAME, NOT is_unique AS NON_UNIQUE,"
                + " NULL AS INDEX_QUALIFIER, index_name AS INDEX_NAME, 3 AS TYPE, ordinal_position AS ORDINAL_POSITION,"
                + " column_name AS COLUMN_NAME, 'A' AS ASC_OR_DESC, NULL AS CARDINALITY, NULL AS PAGES, NULL AS FILTER_CONDITION"
                + " FROM information_schema.indexes WHERE table_name LIKE " + pattern(table) + (unique ? " AND is_unique" : "")
                + " ORDER BY NON_UNIQUE, TYPE, INDEX_NAME, ORDINAL_POSITION");
    }

    @Override
    public ResultSet getTableTypes() throws SQLException {
        return query("SELECT 'TABLE' AS TABLE_TYPE UNION ALL SELECT 'VIEW'");
    }

    @Override
    public ResultSet getSchemas() throws SQLException {
        return empty("TABLE_SCHEM", "TABLE_CATALOG");
    }

    @Override
    public ResultSet getSchemas(String catalog, String schemaPattern) throws SQLException {
        return empty("TABLE_SCHEM", "TABLE_CATALOG");
    }

    @Override
    public ResultSet getCatalogs() throws SQLException {
        return empty("TABLE_CAT");
    }

    @Override
    public ResultSet getProcedures(String catalog, String schemaPattern, String procedureNamePattern) throws SQLException {
        return query("SELECT NULL AS PROCEDURE_CAT, NULL AS PROCEDURE_SCHEM, routine_name AS PROCEDURE_NAME, NULL AS RESERVED1,"
                + " NULL AS RESERVED2, NULL AS RESERVED3, NULL AS REMARKS, 1 AS PROCEDURE_TYPE, routine_name AS SPECIFIC_NAME"
                + " FROM information_schema.routines WHERE routine_type = 'PROCEDURE' AND routine_name LIKE " + pattern(procedureNamePattern)
                + " ORDER BY PROCEDURE_NAME");
    }

    @Override
    public ResultSet getFunctions(String catalog, String schemaPattern, String functionNamePattern) throws SQLException {
        return query("SELECT NULL AS FUNCTION_CAT, NULL AS FUNCTION_SCHEM, routine_name AS FUNCTION_NAME, NULL AS REMARKS,"
                + " 1 AS FUNCTION_TYPE, routine_name AS SPECIFIC_NAME"
                + " FROM information_schema.routines WHERE routine_type = 'FUNCTION' AND routine_name LIKE " + pattern(functionNamePattern)
                + " ORDER BY FUNCTION_NAME");
    }

    @Override
    public ResultSet getTypeInfo() throws SQLException {
        String[][] types = {
            {"INTEGER", "-5", "19", "0"}, {"TEXT", "12", "2147483647", "1"}, {"DOUBLE", "8", "17", "0"}, {"FLOAT", "7", "9", "0"},
            {"YESNO", "16", "1", "0"}, {"DECIMAL", "3", "2147483647", "0"}, {"CURRENCY", "3", "2147483647", "0"},
            {"BIGNUM", "2", "2147483647", "0"}, {"RATIONAL", "1111", "2147483647", "0"}, {"DATETIME", "93", "19", "0"},
            {"TIMESTAMPTZ", "2014", "25", "0"}, {"INTERVAL", "1111", "2147483647", "0"}, {"JSON", "1111", "2147483647", "1"},
        };
        StringBuilder sb = new StringBuilder();
        for (String[] t : types) {
            if (sb.length() > 0) {
                sb.append(" UNION ALL ");
            }
            boolean text = t[3].equals("1");
            sb.append("SELECT '").append(t[0]).append("' AS TYPE_NAME, ").append(t[1]).append(" AS DATA_TYPE, ").append(t[2])
                    .append(" AS PRECISION, ").append(text ? "'''' " : "NULL ").append("AS LITERAL_PREFIX, ").append(text ? "'''' " : "NULL ")
                    .append("AS LITERAL_SUFFIX, NULL AS CREATE_PARAMS, 1 AS NULLABLE, FALSE AS CASE_SENSITIVE, 3 AS SEARCHABLE,")
                    .append(" FALSE AS UNSIGNED_ATTRIBUTE, FALSE AS FIXED_PREC_SCALE, FALSE AS AUTO_INCREMENT, '").append(t[0])
                    .append("' AS LOCAL_TYPE_NAME, 0 AS MINIMUM_SCALE, 0 AS MAXIMUM_SCALE, NULL AS SQL_DATA_TYPE, NULL AS SQL_DATETIME_SUB,")
                    .append(" 10 AS NUM_PREC_RADIX");
        }
        return query(sb.toString());
    }

    @Override
    public ResultSet getTablePrivileges(String catalog, String schemaPattern, String tableNamePattern) throws SQLException {
        return empty("TABLE_CAT", "TABLE_SCHEM", "TABLE_NAME", "GRANTOR", "GRANTEE", "PRIVILEGE", "IS_GRANTABLE");
    }

    @Override
    public ResultSet getColumnPrivileges(String catalog, String schema, String table, String columnNamePattern) throws SQLException {
        return empty("TABLE_CAT", "TABLE_SCHEM", "TABLE_NAME", "COLUMN_NAME", "GRANTOR", "GRANTEE", "PRIVILEGE", "IS_GRANTABLE");
    }

    @Override
    public ResultSet getVersionColumns(String catalog, String schema, String table) throws SQLException {
        return empty("SCOPE", "COLUMN_NAME", "DATA_TYPE", "TYPE_NAME", "COLUMN_SIZE", "BUFFER_LENGTH", "DECIMAL_DIGITS", "PSEUDO_COLUMN");
    }

    @Override
    public ResultSet getBestRowIdentifier(String catalog, String schema, String table, int scope, boolean nullable) throws SQLException {
        return query("SELECT 2 AS SCOPE, k.column_name AS COLUMN_NAME, -5 AS DATA_TYPE, 'INTEGER' AS TYPE_NAME, NULL AS COLUMN_SIZE,"
                + " NULL AS BUFFER_LENGTH, NULL AS DECIMAL_DIGITS, 1 AS PSEUDO_COLUMN"
                + " FROM information_schema.key_column_usage k JOIN information_schema.table_constraints c"
                + " ON c.constraint_name = k.constraint_name AND c.table_name = k.table_name"
                + " WHERE c.constraint_type = 'PRIMARY KEY' AND k.table_name LIKE " + pattern(table) + " ORDER BY k.ordinal_position");
    }

    @Override
    public ResultSet getUDTs(String catalog, String schemaPattern, String typeNamePattern, int[] types) throws SQLException {
        return empty("TYPE_CAT", "TYPE_SCHEM", "TYPE_NAME", "CLASS_NAME", "DATA_TYPE", "REMARKS", "BASE_TYPE");
    }

    @Override
    public ResultSet getSuperTypes(String catalog, String schemaPattern, String typeNamePattern) throws SQLException {
        return empty("TYPE_CAT", "TYPE_SCHEM", "TYPE_NAME", "SUPERTYPE_CAT", "SUPERTYPE_SCHEM", "SUPERTYPE_NAME");
    }

    @Override
    public ResultSet getSuperTables(String catalog, String schemaPattern, String tableNamePattern) throws SQLException {
        return empty("TABLE_CAT", "TABLE_SCHEM", "TABLE_NAME", "SUPERTABLE_NAME");
    }

    @Override
    public ResultSet getProcedureColumns(String catalog, String schemaPattern, String procedureNamePattern, String columnNamePattern)
            throws SQLException {
        return query("SELECT NULL AS PROCEDURE_CAT, NULL AS PROCEDURE_SCHEM, p.routine_name AS PROCEDURE_NAME, p.parameter_name AS COLUMN_NAME,"
                + " 1 AS COLUMN_TYPE, " + SQL_TYPE.replace("data_type", "p.data_type") + " AS DATA_TYPE, COALESCE(p.data_type, 'ANY') AS TYPE_NAME,"
                + " NULL AS PRECISION, NULL AS LENGTH, NULL AS SCALE, 10 AS RADIX, 2 AS NULLABLE, NULL AS REMARKS, NULL AS COLUMN_DEF,"
                + " NULL AS SQL_DATA_TYPE, NULL AS SQL_DATETIME_SUB, NULL AS CHAR_OCTET_LENGTH, p.ordinal_position AS ORDINAL_POSITION,"
                + " 'YES' AS IS_NULLABLE, p.routine_name AS SPECIFIC_NAME"
                + " FROM information_schema.parameters p WHERE p.routine_name LIKE " + pattern(procedureNamePattern)
                + " AND p.parameter_name LIKE " + pattern(columnNamePattern) + " ORDER BY PROCEDURE_NAME, ORDINAL_POSITION");
    }

    @Override
    public ResultSet getAttributes(String catalog, String schemaPattern, String typeNamePattern, String attributeNamePattern) throws SQLException {
        return empty("TYPE_CAT", "TYPE_SCHEM", "TYPE_NAME", "ATTR_NAME", "DATA_TYPE", "ATTR_TYPE_NAME", "ATTR_SIZE");
    }

    @Override
    public ResultSet getClientInfoProperties() throws SQLException {
        return empty("NAME", "MAX_LEN", "DEFAULT_VALUE", "DESCRIPTION");
    }

    @Override
    public ResultSet getPseudoColumns(String catalog, String schemaPattern, String tableNamePattern, String columnNamePattern) throws SQLException {
        return empty("TABLE_CAT", "TABLE_SCHEM", "TABLE_NAME", "COLUMN_NAME", "DATA_TYPE");
    }

    @Override
    public ResultSet getFunctionColumns(String catalog, String schemaPattern, String functionNamePattern, String columnNamePattern) throws SQLException {
        return empty("FUNCTION_CAT", "FUNCTION_SCHEM", "FUNCTION_NAME", "COLUMN_NAME", "COLUMN_TYPE", "DATA_TYPE");
    }

    // ----- the product and the driver ---------------------------------------------------------

    @Override
    public String getDatabaseProductName() {
        return "Accessor";
    }

    @Override
    public String getDatabaseProductVersion() {
        return Native.version();
    }

    @Override
    public int getDatabaseMajorVersion() {
        return Native.versionNumber() / 10_000;
    }

    @Override
    public int getDatabaseMinorVersion() {
        return Native.versionNumber() / 100 % 100;
    }

    @Override
    public String getDriverName() {
        return "Accessor JDBC driver";
    }

    @Override
    public String getDriverVersion() {
        return Native.version();
    }

    @Override
    public int getDriverMajorVersion() {
        return getDatabaseMajorVersion();
    }

    @Override
    public int getDriverMinorVersion() {
        return getDatabaseMinorVersion();
    }

    @Override
    public int getJDBCMajorVersion() {
        return 4;
    }

    @Override
    public int getJDBCMinorVersion() {
        return 3;
    }

    @Override
    public String getURL() {
        return conn.url();
    }

    @Override
    public String getUserName() {
        return "";
    }

    @Override
    public Connection getConnection() {
        return conn;
    }

    // ----- identifiers and words --------------------------------------------------------------

    @Override
    public String getIdentifierQuoteString() {
        return " "; // names are quoted [like this], which a single string cannot say
    }

    @Override
    public String getSQLKeywords() {
        return "COUNTER,CURRENCY,DATETIME,YESNO,TOP,PIVOT,UNPIVOT,TRANSFORM,RETURNING,LATERAL,SEQUENCE";
    }

    @Override
    public String getNumericFunctions() {
        return "ABS,ACOS,ASIN,ATAN,ATAN2,CEIL,COS,DEGREES,EXP,FLOOR,LOG,LOG10,MOD,PI,POWER,RADIANS,RANDOM,ROUND,SGN,SIN,SQR,TAN,TRUNC";
    }

    @Override
    public String getStringFunctions() {
        return "CONCAT,INITCAP,INSTR,LCASE,LEFT,LEN,LPAD,LTRIM,MID,OVERLAY,REPEAT,REPLACE,REVERSE,RIGHT,RPAD,RTRIM,TRANSLATE,TRIM,UCASE";
    }

    @Override
    public String getSystemFunctions() {
        return "COALESCE,IIF,ISNULL,NULLIF,NZ";
    }

    @Override
    public String getTimeDateFunctions() {
        return "DATE,DATEADD,DATEDIFF,DATEPART,DATESERIAL,DAY,HOUR,MINUTE,MONTH,NOW,SECOND,WEEKDAY,YEAR";
    }

    @Override
    public String getSearchStringEscape() {
        return "\\";
    }

    @Override
    public String getExtraNameCharacters() {
        return "";
    }

    @Override
    public String getSchemaTerm() {
        return "schema";
    }

    @Override
    public String getProcedureTerm() {
        return "procedure";
    }

    @Override
    public String getCatalogTerm() {
        return "catalog";
    }

    @Override
    public String getCatalogSeparator() {
        return ".";
    }

    @Override
    public boolean isCatalogAtStart() {
        return false;
    }

    // ----- what it can do ---------------------------------------------------------------------

    @Override
    public boolean allProceduresAreCallable() {
        return true;
    }

    @Override
    public boolean allTablesAreSelectable() {
        return true;
    }

    @Override
    public boolean isReadOnly() {
        return false;
    }

    @Override
    public boolean nullsAreSortedHigh() {
        return false;
    }

    @Override
    public boolean nullsAreSortedLow() {
        return true;
    }

    @Override
    public boolean nullsAreSortedAtStart() {
        return false;
    }

    @Override
    public boolean nullsAreSortedAtEnd() {
        return false;
    }

    @Override
    public boolean usesLocalFiles() {
        return true;
    }

    @Override
    public boolean usesLocalFilePerTable() {
        return false;
    }

    @Override
    public boolean supportsMixedCaseIdentifiers() {
        return false;
    }

    @Override
    public boolean storesUpperCaseIdentifiers() {
        return false;
    }

    @Override
    public boolean storesLowerCaseIdentifiers() {
        return false;
    }

    @Override
    public boolean storesMixedCaseIdentifiers() {
        return true;
    }

    @Override
    public boolean supportsMixedCaseQuotedIdentifiers() {
        return false;
    }

    @Override
    public boolean storesUpperCaseQuotedIdentifiers() {
        return false;
    }

    @Override
    public boolean storesLowerCaseQuotedIdentifiers() {
        return false;
    }

    @Override
    public boolean storesMixedCaseQuotedIdentifiers() {
        return true;
    }

    @Override
    public boolean supportsAlterTableWithAddColumn() {
        return true;
    }

    @Override
    public boolean supportsAlterTableWithDropColumn() {
        return true;
    }

    @Override
    public boolean supportsColumnAliasing() {
        return true;
    }

    @Override
    public boolean nullPlusNonNullIsNull() {
        return true;
    }

    @Override
    public boolean supportsConvert() {
        return false;
    }

    @Override
    public boolean supportsConvert(int fromType, int toType) {
        return false;
    }

    @Override
    public boolean supportsTableCorrelationNames() {
        return true;
    }

    @Override
    public boolean supportsDifferentTableCorrelationNames() {
        return false;
    }

    @Override
    public boolean supportsExpressionsInOrderBy() {
        return true;
    }

    @Override
    public boolean supportsOrderByUnrelated() {
        return true;
    }

    @Override
    public boolean supportsGroupBy() {
        return true;
    }

    @Override
    public boolean supportsGroupByUnrelated() {
        return false;
    }

    @Override
    public boolean supportsGroupByBeyondSelect() {
        return true;
    }

    @Override
    public boolean supportsLikeEscapeClause() {
        return true;
    }

    @Override
    public boolean supportsMultipleResultSets() {
        return false;
    }

    @Override
    public boolean supportsMultipleTransactions() {
        return false;
    }

    @Override
    public boolean supportsNonNullableColumns() {
        return true;
    }

    @Override
    public boolean supportsMinimumSQLGrammar() {
        return true;
    }

    @Override
    public boolean supportsCoreSQLGrammar() {
        return true;
    }

    @Override
    public boolean supportsExtendedSQLGrammar() {
        return false;
    }

    @Override
    public boolean supportsANSI92EntryLevelSQL() {
        return false;
    }

    @Override
    public boolean supportsANSI92IntermediateSQL() {
        return false;
    }

    @Override
    public boolean supportsANSI92FullSQL() {
        return false;
    }

    @Override
    public boolean supportsIntegrityEnhancementFacility() {
        return true;
    }

    @Override
    public boolean supportsOuterJoins() {
        return true;
    }

    @Override
    public boolean supportsFullOuterJoins() {
        return true;
    }

    @Override
    public boolean supportsLimitedOuterJoins() {
        return true;
    }

    @Override
    public boolean supportsSchemasInDataManipulation() {
        return false;
    }

    @Override
    public boolean supportsSchemasInProcedureCalls() {
        return false;
    }

    @Override
    public boolean supportsSchemasInTableDefinitions() {
        return false;
    }

    @Override
    public boolean supportsSchemasInIndexDefinitions() {
        return false;
    }

    @Override
    public boolean supportsSchemasInPrivilegeDefinitions() {
        return false;
    }

    @Override
    public boolean supportsCatalogsInDataManipulation() {
        return false;
    }

    @Override
    public boolean supportsCatalogsInProcedureCalls() {
        return false;
    }

    @Override
    public boolean supportsCatalogsInTableDefinitions() {
        return false;
    }

    @Override
    public boolean supportsCatalogsInIndexDefinitions() {
        return false;
    }

    @Override
    public boolean supportsCatalogsInPrivilegeDefinitions() {
        return false;
    }

    @Override
    public boolean supportsPositionedDelete() {
        return false;
    }

    @Override
    public boolean supportsPositionedUpdate() {
        return false;
    }

    @Override
    public boolean supportsSelectForUpdate() {
        return false;
    }

    @Override
    public boolean supportsStoredProcedures() {
        return true;
    }

    @Override
    public boolean supportsSubqueriesInComparisons() {
        return true;
    }

    @Override
    public boolean supportsSubqueriesInExists() {
        return true;
    }

    @Override
    public boolean supportsSubqueriesInIns() {
        return true;
    }

    @Override
    public boolean supportsSubqueriesInQuantifieds() {
        return true;
    }

    @Override
    public boolean supportsCorrelatedSubqueries() {
        return true;
    }

    @Override
    public boolean supportsUnion() {
        return true;
    }

    @Override
    public boolean supportsUnionAll() {
        return true;
    }

    @Override
    public boolean supportsOpenCursorsAcrossCommit() {
        return false;
    }

    @Override
    public boolean supportsOpenCursorsAcrossRollback() {
        return false;
    }

    @Override
    public boolean supportsOpenStatementsAcrossCommit() {
        return true;
    }

    @Override
    public boolean supportsOpenStatementsAcrossRollback() {
        return true;
    }

    @Override
    public boolean doesMaxRowSizeIncludeBlobs() {
        return true;
    }

    @Override
    public boolean supportsTransactions() {
        return true;
    }

    @Override
    public boolean supportsTransactionIsolationLevel(int level) {
        return level == Connection.TRANSACTION_SERIALIZABLE;
    }

    @Override
    public int getDefaultTransactionIsolation() {
        return Connection.TRANSACTION_SERIALIZABLE;
    }

    @Override
    public boolean supportsDataDefinitionAndDataManipulationTransactions() {
        return true;
    }

    @Override
    public boolean supportsDataManipulationTransactionsOnly() {
        return false;
    }

    @Override
    public boolean dataDefinitionCausesTransactionCommit() {
        return false;
    }

    @Override
    public boolean dataDefinitionIgnoredInTransactions() {
        return false;
    }

    @Override
    public boolean supportsResultSetType(int type) {
        return type == ResultSet.TYPE_FORWARD_ONLY || type == ResultSet.TYPE_SCROLL_INSENSITIVE;
    }

    @Override
    public boolean supportsResultSetConcurrency(int type, int concurrency) {
        return supportsResultSetType(type) && concurrency == ResultSet.CONCUR_READ_ONLY;
    }

    @Override
    public boolean supportsResultSetHoldability(int holdability) {
        return holdability == ResultSet.CLOSE_CURSORS_AT_COMMIT;
    }

    @Override
    public int getResultSetHoldability() {
        return ResultSet.CLOSE_CURSORS_AT_COMMIT;
    }

    @Override
    public boolean ownUpdatesAreVisible(int type) {
        return false;
    }

    @Override
    public boolean ownDeletesAreVisible(int type) {
        return false;
    }

    @Override
    public boolean ownInsertsAreVisible(int type) {
        return false;
    }

    @Override
    public boolean othersUpdatesAreVisible(int type) {
        return false;
    }

    @Override
    public boolean othersDeletesAreVisible(int type) {
        return false;
    }

    @Override
    public boolean othersInsertsAreVisible(int type) {
        return false;
    }

    @Override
    public boolean updatesAreDetected(int type) {
        return false;
    }

    @Override
    public boolean deletesAreDetected(int type) {
        return false;
    }

    @Override
    public boolean insertsAreDetected(int type) {
        return false;
    }

    @Override
    public boolean supportsBatchUpdates() {
        return true;
    }

    @Override
    public boolean supportsSavepoints() {
        return true;
    }

    @Override
    public boolean supportsNamedParameters() {
        return false;
    }

    @Override
    public boolean supportsMultipleOpenResults() {
        return false;
    }

    @Override
    public boolean supportsGetGeneratedKeys() {
        return true;
    }

    @Override
    public boolean locatorsUpdateCopy() {
        return false;
    }

    @Override
    public boolean supportsStatementPooling() {
        return false;
    }

    @Override
    public RowIdLifetime getRowIdLifetime() {
        return RowIdLifetime.ROWID_UNSUPPORTED;
    }

    @Override
    public boolean supportsStoredFunctionsUsingCallSyntax() {
        return false;
    }

    @Override
    public boolean autoCommitFailureClosesAllResultSets() {
        return false;
    }

    @Override
    public boolean generatedKeyAlwaysReturned() {
        return false;
    }

    @Override
    public boolean supportsRefCursors() {
        return false;
    }

    @Override
    public boolean supportsSharding() {
        return false;
    }

    @Override
    public int getSQLStateType() {
        return DatabaseMetaData.sqlStateSQL;
    }

    // Limits: none.
    @Override
    public int getMaxBinaryLiteralLength() {
        return 0;
    }

    @Override
    public int getMaxCharLiteralLength() {
        return 0;
    }

    @Override
    public int getMaxColumnNameLength() {
        return 0;
    }

    @Override
    public int getMaxColumnsInGroupBy() {
        return 0;
    }

    @Override
    public int getMaxColumnsInIndex() {
        return 0;
    }

    @Override
    public int getMaxColumnsInOrderBy() {
        return 0;
    }

    @Override
    public int getMaxColumnsInSelect() {
        return 0;
    }

    @Override
    public int getMaxColumnsInTable() {
        return 0;
    }

    @Override
    public int getMaxConnections() {
        return 1;
    }

    @Override
    public int getMaxCursorNameLength() {
        return 0;
    }

    @Override
    public int getMaxIndexLength() {
        return 0;
    }

    @Override
    public int getMaxSchemaNameLength() {
        return 0;
    }

    @Override
    public int getMaxProcedureNameLength() {
        return 0;
    }

    @Override
    public int getMaxCatalogNameLength() {
        return 0;
    }

    @Override
    public int getMaxRowSize() {
        return 0;
    }

    @Override
    public int getMaxStatementLength() {
        return 0;
    }

    @Override
    public int getMaxStatements() {
        return 0;
    }

    @Override
    public int getMaxTableNameLength() {
        return 0;
    }

    @Override
    public int getMaxTablesInSelect() {
        return 0;
    }

    @Override
    public int getMaxUserNameLength() {
        return 0;
    }
}
