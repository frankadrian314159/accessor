(ns accessor.honey-test
  "HoneySQL 2 (SQL as data) with next.jdbc, against Accessor's JDBC driver. Run with run.sh / run.ps1.

  Accessor quotes names as [name], as SQL Server does, so the :sqlserver dialect is the one to format with."
  (:require [clojure.test :refer [deftest is testing run-tests]]
            [honey.sql :as sql]
            [honey.sql.helpers :as h]
            [next.jdbc :as jdbc]
            [next.jdbc.result-set :as rs]))

(def ^:private opts {:dialect :sqlserver :quoted true})

(defn fmt [m] (sql/format m opts))

(defn q
  "Runs a HoneySQL map and returns maps with plain keys."
  [con m]
  (jdbc/execute! con (fmt m) {:builder-fn rs/as-unqualified-maps}))

(defn exec! [con m] (jdbc/execute! con (fmt m) {:builder-fn rs/as-unqualified-maps}))

(defmacro with-db [[con] & body]
  `(with-open [~con (jdbc/get-connection {:jdbcUrl "jdbc:accessor:mem:"})]
     ~@body))

(defn- people-db [con]
  (exec! con {:create-table :people
             :with-columns [[:id :counter [:primary-key]]
                            [:name :text [:not nil]]
                            [:age :integer]
                            [:dept :text]]})
  (exec! con {:insert-into :people
             :columns [:name :age :dept]
             :values [["Ada" 36 "eng"] ["Grace" 45 "eng"] ["Edsger" 72 "research"]
                      ["Barbara" 50 "research"] ["Linus" nil "eng"]]}))

(deftest what-honeysql-writes
  (testing "names are quoted in square brackets and values are parameters"
    (is (= ["SELECT [name] FROM [people] WHERE [age] > ?" 40]
           (fmt {:select [:name] :from [:people] :where [:> :age 40]}))))
  (testing "the helpers build the same maps"
    (is (= (fmt {:select [:name] :from [:people] :where [:= :dept "eng"]})
           (fmt (-> (h/select :name) (h/from :people) (h/where [:= :dept "eng"]))))))
  (testing "IN lists, LIKE and BETWEEN"
    (is (= ["SELECT * FROM [t] WHERE ([a] IN (?, ?)) AND ([b] LIKE ?) AND [c] BETWEEN ? AND ?" 1 2 "x%" 3 4]
           (fmt {:select [:*] :from [:t]
                 :where [:and [:in :a [1 2]] [:like :b "x%"] [:between :c 3 4]]})))))

(deftest rows-in-and-out
  (with-db [con]
    (people-db con)
    (testing "select with where, order by"
      (is (= ["Edsger" "Barbara" "Grace"]
             (map :name (q con {:select [:name] :from [:people]
                                :where [:> :age 40] :order-by [[:age :desc]]})))))
    (testing "and / or / in / like"
      (is (= ["Ada" "Grace"]
             (map :name (q con {:select [:name] :from [:people]
                                :where [:and [:= :dept "eng"] [:in :age [36 45]]]
                                :order-by [:id]}))))
      (is (= ["Edsger"]
             (map :name (q con {:select [:name] :from [:people] :where [:like :name "Eds%"]})))))
    (testing "NULL: is-null tests, and a missing value reads as nil"
      (is (= ["Linus"] (map :name (q con {:select [:name] :from [:people] :where [:= :age nil]}))))
      (is (nil? (:age (first (q con {:select [:age] :from [:people] :where [:= :name "Linus"]}))))))
    (testing "group by, aggregates, having"
      (is (= [{:dept "eng" :n 3} {:dept "research" :n 2}]
             (q con {:select [:dept [[:count :*] :n]] :from [:people]
                     :group-by [:dept] :order-by [:dept]})))
      (is (= [{:dept "research" :oldest 72}]
             (q con {:select [:dept [[:max :age] :oldest]] :from [:people]
                     :group-by [:dept] :having [:> [:min :age] 40]}))))
    (testing "update and delete"
      (exec! con {:update :people :set {:age 37} :where [:= :name "Ada"]})
      (is (= 37 (:age (first (q con {:select [:age] :from [:people] :where [:= :name "Ada"]})))))
      (exec! con {:delete-from :people :where [:= :dept "research"]})
      (is (= 3 (:n (first (q con {:select [[[:count :*] :n]] :from [:people]}))))))))

(deftest joins-subqueries-and-ctes
  (with-db [con]
    (people-db con)
    (exec! con {:create-table :depts :with-columns [[:code :text [:primary-key]] [:floor :integer]]})
    (exec! con {:insert-into :depts :columns [:code :floor] :values [["eng" 2] ["research" 3]]})
    (testing "join"
      (is (= [{:name "Edsger" :floor 3}]
             (q con {:select [:p.name :d.floor] :from [[:people :p]]
                     :join [[:depts :d] [:= :p.dept :d.code]]
                     :where [:= :p.name "Edsger"]}))))
    (testing "subquery"
      (is (= ["Edsger" "Barbara"]
             (map :name (q con {:select [:name] :from [:people]
                                :where [:in :dept {:select [:code] :from [:depts] :where [:= :floor 3]}]
                                :order-by [:id]})))))
    (testing "a common table expression"
      (is (= [{:n 3}]
             (q con {:with [[:eng {:select [:*] :from [:people] :where [:= :dept "eng"]}]]
                     :select [[[:count :*] :n]] :from [:eng]}))))
    (testing "union"
      (is (= ["Ada" "Edsger"]
             (map :name (q con {:union-all [{:select [:name] :from [:people] :where [:= :name "Ada"]}
                                            {:select [:name] :from [:people] :where [:= :name "Edsger"]}]})))))))

(deftest returning-and-upserts
  (with-db [con]
    (people-db con)
    (testing "RETURNING hands back the new row"
      (is (= [{:id 6 :name "Alan"}]
             (exec! con {:insert-into :people :columns [:name :age] :values [["Alan" 41]]
                        :returning [:id :name]}))))
    (testing "ON CONFLICT ... DO UPDATE"
      (exec! con {:insert-into :people :columns [:id :name :age] :values [[1 "Ada" 99]]
                 :on-conflict :id :do-update-set {:age 99}})
      (is (= 99 (:age (first (q con {:select [:age] :from [:people] :where [:= :id 1]})))))
      (is (= 6 (:n (first (q con {:select [[[:count :*] :n]] :from [:people]}))))))))

(deftest honeysql-with-transactions
  (with-db [con]
    (people-db con)
    (is (thrown? Exception
                 (jdbc/with-transaction [tx con]
                   (exec! tx {:update :people :set {:age 0}})
                   (exec! tx {:insert-into :people :columns [:id :name] :values [[1 "duplicate"]]}))))
    (is (= [36 45 72 50] (map :age (q con {:select [:age] :from [:people]
                                           :where [:<> :age nil] :order-by [:id]}))))))

(defn -main [& _]
  (let [{:keys [fail error]} (run-tests 'accessor.honey-test)]
    (shutdown-agents)
    (System/exit (if (zero? (+ fail error)) 0 1))))
