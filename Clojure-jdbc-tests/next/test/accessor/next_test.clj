(ns accessor.next-test
  "next.jdbc against Accessor's JDBC driver. Run with run.sh / run.ps1 (or `clojure -M:test` in this folder).

  A `jdbc:accessor:mem:` database is private to the connection that opened it, so each test opens one
  connection and uses it throughout."
  (:require [clojure.test :refer [deftest is testing run-tests]]
            [next.jdbc :as jdbc]
            [next.jdbc.result-set :as rs]
            [next.jdbc.sql :as sql])
  (:import (java.math BigDecimal)
           (java.sql SQLException)))

(def ^:private url "jdbc:accessor:mem:")

(defmacro with-db
  "Runs body with `con` bound to a connection to a fresh in-memory database."
  [[con] & body]
  `(with-open [~con (jdbc/get-connection {:jdbcUrl url})]
     ~@body))

(defn- sqlstate [f]
  (try (f) nil
       (catch SQLException e (.getSQLState e))))

(deftest connect-and-ask
  (with-db [con]
    (is (= {:one 1} (jdbc/execute-one! con ["SELECT 1 AS one"] {:builder-fn rs/as-unqualified-maps})))
    (is (= "Accessor" (.getDatabaseProductName (.getMetaData con))))))

(deftest tables-and-rows
  (with-db [con]
    (jdbc/execute! con ["CREATE TABLE people (id COUNTER PRIMARY KEY, name TEXT NOT NULL, age INTEGER)"])
    (testing "execute-one! with :return-keys gives the generated key"
      (let [row (jdbc/execute-one! con ["INSERT INTO people (name, age) VALUES (?, ?)" "Ada" 36]
                                   {:return-keys true})]
        (is (some? row))
        (is (= 1 (first (vals row))))))
    (testing "next.jdbc.sql helpers"
      (sql/insert! con :people {:name "Grace" :age 45})
      (sql/insert-multi! con :people [:name :age] [["Edsger" nil] ["Barbara" 50]])
      (is (= 4 (:c (jdbc/execute-one! con ["SELECT COUNT(*) AS c FROM people"]
                                      {:builder-fn rs/as-unqualified-maps}))))
      (is (= ["Grace"]
             (map :name (sql/find-by-keys con :people {:age 45} {:builder-fn rs/as-unqualified-maps}))))
      (sql/update! con :people {:age 37} {:name "Ada"})
      (is (= 37 (:age (first (sql/find-by-keys con :people {:name "Ada"}
                                               {:builder-fn rs/as-unqualified-maps})))))
      (sql/delete! con :people {:name "Edsger"})
      (is (= ["Ada" "Barbara" "Grace"]
             (sort (map :name (jdbc/execute! con ["SELECT name FROM people"]
                                             {:builder-fn rs/as-unqualified-maps}))))))
    (testing "NULL comes back as nil"
      (sql/insert! con :people {:name "Linus" :age nil})
      (is (nil? (:age (jdbc/execute-one! con ["SELECT age FROM people WHERE name = ?" "Linus"]
                                         {:builder-fn rs/as-unqualified-maps})))))))

(deftest plan-reduces-without-building-maps
  (with-db [con]
    (jdbc/execute! con ["CREATE TABLE n (v INTEGER)"])
    (jdbc/execute! con ["INSERT INTO n VALUES (1), (2), (3), (4)"])
    (is (= 10 (reduce (fn [acc row] (+ acc (:v row)))
                      0
                      (jdbc/plan con ["SELECT v FROM n"]))))))

(deftest types-come-back-exactly
  (with-db [con]
    (jdbc/execute! con ["CREATE TABLE t (d DECIMAL, f DOUBLE, flag YESNO, txt TEXT)"])
    (jdbc/execute! con ["INSERT INTO t VALUES (?, ?, ?, ?)" (BigDecimal. "0.1") 2.5 true "héllo 世界"])
    (let [row (jdbc/execute-one! con ["SELECT d + 0.2 AS sum, f, flag, txt FROM t"]
                                 {:builder-fn rs/as-unqualified-maps})]
      (is (= (BigDecimal. "0.3") (:sum row)) "0.1 + 0.2 is exactly 0.3")
      (is (= 2.5 (:f row)))
      (is (true? (boolean (:flag row))))
      (is (= "héllo 世界" (:txt row))))))

(deftest transactions
  (with-db [con]
    (jdbc/execute! con ["CREATE TABLE acct (id INTEGER PRIMARY KEY, bal INTEGER)"])
    (jdbc/execute! con ["INSERT INTO acct VALUES (1, 100), (2, 100)"])
    (letfn [(bals [] (mapv :bal (jdbc/execute! con ["SELECT bal FROM acct ORDER BY id"]
                                               {:builder-fn rs/as-unqualified-maps})))]
      (testing "commit"
        (jdbc/with-transaction [tx con]
          (jdbc/execute! tx ["UPDATE acct SET bal = bal - 30 WHERE id = 1"])
          (jdbc/execute! tx ["UPDATE acct SET bal = bal + 30 WHERE id = 2"]))
        (is (= [70 130] (bals))))
      (testing "an exception rolls everything back"
        (is (thrown? Exception
                     (jdbc/with-transaction [tx con]
                       (jdbc/execute! tx ["UPDATE acct SET bal = 0"])
                       (throw (ex-info "stop" {})))))
        (is (= [70 130] (bals))))
      (testing ":rollback-only"
        (jdbc/with-transaction [tx con {:rollback-only true}]
          (jdbc/execute! tx ["DELETE FROM acct"]))
        (is (= [70 130] (bals)))))))

(deftest batches-are-all-or-nothing
  (with-db [con]
    (jdbc/execute! con ["CREATE TABLE u (id INTEGER PRIMARY KEY)"])
    (is (thrown? Exception (jdbc/execute-batch! (jdbc/prepare con ["INSERT INTO u VALUES (?)"])
                                                [[1] [2] [2]] {})))
    (is (= 0 (:c (jdbc/execute-one! con ["SELECT COUNT(*) AS c FROM u"]
                                    {:builder-fn rs/as-unqualified-maps}))))
    (jdbc/execute-batch! (jdbc/prepare con ["INSERT INTO u VALUES (?)"]) [[1] [2] [3]] {})
    (is (= 3 (:c (jdbc/execute-one! con ["SELECT COUNT(*) AS c FROM u"]
                                    {:builder-fn rs/as-unqualified-maps}))))))

(deftest errors-carry-sql-states
  (with-db [con]
    (jdbc/execute! con ["CREATE TABLE e (id INTEGER PRIMARY KEY)"])
    (is (= "42000" (sqlstate #(jdbc/execute! con ["SELEC 1"]))))
    (is (= "42S02" (sqlstate #(jdbc/execute! con ["SELECT * FROM nope"]))))
    (jdbc/execute! con ["INSERT INTO e VALUES (1)"])
    (is (= "23000" (sqlstate #(jdbc/execute! con ["INSERT INTO e VALUES (1)"]))))))

(deftest metadata
  (with-db [con]
    (jdbc/execute! con ["CREATE TABLE Orders (ID COUNTER PRIMARY KEY, Customer TEXT)"])
    (let [md (.getMetaData con)
          names (with-open [rs (.getTables md nil nil "%" (into-array String ["TABLE"]))]
                  (loop [acc []] (if (.next rs) (recur (conj acc (.getString rs "TABLE_NAME"))) acc)))]
      (is (some #(.equalsIgnoreCase "Orders" %) names)))))

(defn -main [& _]
  (let [{:keys [fail error]} (run-tests 'accessor.next-test)]
    (shutdown-agents)
    (System/exit (if (zero? (+ fail error)) 0 1))))
